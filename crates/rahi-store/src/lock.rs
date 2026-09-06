//! Distributed leases and the fencing predicate (spec 012 B-1, B-2).
//!
//! hiqlite's distributed lock gives mutual exclusion with a hardcoded
//! ten-second TTL, which means a holder can lose its lease while it still
//! believes it holds one. Fencing is therefore mandatory rather than
//! hardening: every lease-guarded write carries a token, the store refuses a
//! write whose token has been superseded, and the refusal rolls the whole
//! batch back.
//!
//! A lease guards a key, and a token is only comparable with other tokens of
//! the same key. One lease key guards one resource set; two keys writing the
//! same `fence` column would compare two unrelated sequences.

use std::sync::atomic::{AtomicBool, Ordering};

use rahi_types::{Error, FenceToken};
use serde::Deserialize;

use crate::error::map;
use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::{ExecuteResult, Statement};

/// The table that mints fencing tokens, created by
/// [`crate::coordination_migration`].
///
/// One row per lease key holding the highest token ever handed out for it.
/// It lives in the SQL group, not the cache group, because the cache group is
/// derived state that a clear, a TTL, or a restore may empty, and a token
/// sequence that lost its place would hand out tokens below the ones already
/// recorded in `fence` columns, refusing every later write.
pub const FENCE_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS lease_fence (\
    lease_key TEXT PRIMARY KEY, \
    token INTEGER NOT NULL)";

/// Mint the next token for a key. Serialised by the lock the caller holds.
const BUMP_SQL: &str = "INSERT INTO lease_fence (lease_key, token) VALUES ($1, 1) \
    ON CONFLICT(lease_key) DO UPDATE SET token = lease_fence.token + 1";

const READ_SQL: &str = "SELECT token FROM lease_fence WHERE lease_key = $1";

/// The column as SQLite names it in the constraint message the guard
/// provokes ("NOT NULL constraint failed: lease_fence.token"). hiqlite
/// reports a failed statement inside a transaction as its own error kind
/// rather than as a constraint violation, so the guard is recognised by the
/// column it names, which no other statement in the batch can touch.
const FENCE_COLUMN: &str = "lease_fence.token";

/// The guard every [`StoreHandle::fenced_txn`] runs first.
///
/// A token that is still the newest one for its key writes the column back
/// unchanged; a superseded token evaluates to `NULL` against a `NOT NULL`
/// column, which SQLite refuses. SQLite has no `RAISE` outside a trigger, and
/// a predicate that merely matches no row would let the rest of the batch
/// commit, so the constraint violation is the abort: it rolls the batch back
/// and surfaces as [`Error::Conflict`].
const GUARD_SQL: &str = "UPDATE lease_fence \
    SET token = CASE WHEN token <= $1 THEN token ELSE NULL END \
    WHERE lease_key = $2";

/// The lease TTL hiqlite hardcodes. Documented, never configurable.
pub const LEASE_TTL_SECONDS: u64 = 10;

#[derive(Deserialize)]
struct TokenRow {
    token: i64,
}

/// A held distributed lease and the fencing token minted with it.
///
/// Acquired by [`StoreHandle::lease`]. Released by [`Lease::release`]; the
/// handle's `Drop` is a backstop, not the mechanism, because a drop cannot
/// report a failure and cannot be awaited.
///
/// The lease expires after [`LEASE_TTL_SECONDS`] whether or not the holder is
/// finished, so work is chunked or the lease re-acquired, never assumed to
/// fit. A holder that has been superseded is not told; it finds out when
/// [`StoreHandle::fenced_txn`] returns [`Error::Conflict`].
pub struct Lease {
    /// The fencing token: strictly increasing per key, and the value the
    /// guarded rows record in their `fence` column.
    pub token: FenceToken,
    key: String,
    lock: Option<hiqlite::Lock>,
    superseded: AtomicBool,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("key", &self.key)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Lease {
    /// The key this lease holds.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Hand the lease back.
    ///
    /// Drops hiqlite's lock, which releases it and wakes whoever is queued on
    /// the key. hiqlite performs the release on a task of its own and offers
    /// no acknowledgement, so this call cannot confirm that the release
    /// landed; a caller that must observe the handover observes it by
    /// acquiring again.
    pub async fn release(mut self) {
        self.hand_back();
    }

    /// Give up the lock, unless this lease has already been superseded.
    ///
    /// A superseded lease no longer owns the key, and hiqlite's lock handler
    /// panics when it is told to release a lock another holder now owns,
    /// which would take the node's whole locking subsystem down on exactly
    /// the path fencing exists for. There is nothing to hand back in that
    /// case, so the lock is leaked instead: the new holder already has it,
    /// and what leaks is one key string and one client handle.
    fn hand_back(&mut self) {
        let Some(lock) = self.lock.take() else {
            return;
        };
        if self.superseded.load(Ordering::Relaxed) {
            std::mem::forget(lock);
        } else {
            drop(lock);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.hand_back();
    }
}

impl StoreHandle {
    /// Acquire the distributed lease for `key` and mint its fencing token.
    ///
    /// Waits for the current holder: hiqlite queues the request and grants it
    /// when the holder releases, or to whoever asks after the ten-second TTL
    /// has passed. The token is minted from `lease_fence` while the lock is
    /// held, so tokens for one key are strictly increasing, and it is read
    /// back with `query_consistent` because a stale token is a wrong
    /// decision, not a slow one.
    ///
    /// # Errors
    ///
    /// [`Error::Upstream`] when the cache group cannot grant the lock;
    /// [`Error::Validation`] when `lease_fence` is missing (the app has not
    /// run [`crate::coordination_migration`]); [`Error::Integrity`] when the
    /// minted token is not a positive integer.
    pub async fn lease(&self, key: &str) -> Result<Lease, Error> {
        let lock = self.client().lock(key.to_owned()).await.map_err(map)?;
        let token = self.mint_token(key).await?;
        Ok(Lease {
            token,
            key: key.to_owned(),
            lock: Some(lock),
            superseded: AtomicBool::new(false),
        })
    }

    async fn mint_token(&self, key: &str) -> Result<FenceToken, Error> {
        self.txn(vec![Statement::with_params(
            BUMP_SQL,
            vec![Value::from(key)],
        )])
        .await?;
        let rows: Vec<TokenRow> = self
            .query_consistent(READ_SQL, vec![Value::from(key)])
            .await?;
        let raw = rows
            .first()
            .map(|r| r.token)
            .ok_or_else(|| Error::Integrity(format!("lease {key} minted no fencing token")))?;
        u64::try_from(raw)
            .map(FenceToken::new)
            .map_err(|_| Error::Integrity(format!("lease {key} minted a negative token {raw}")))
    }

    /// Run a batch under a lease, with every statement fenced.
    ///
    /// The batch is prefixed with the fence guard and each statement is
    /// rewritten by [`Statement::fenced`], so a superseded holder's writes
    /// affect no row and the batch rolls back whole. The returned results are
    /// the caller's statements, in order; the guard's result is not included.
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] when the lease has been superseded or has been
    /// released and re-acquired elsewhere; [`Error::Validation`] when a
    /// statement is not an `UPDATE` or a `DELETE`; otherwise the failing
    /// statement's error, with the batch rolled back.
    pub async fn fenced_txn(
        &self,
        lease: &Lease,
        statements: Vec<Statement>,
    ) -> Result<Vec<ExecuteResult>, Error> {
        if statements.is_empty() {
            return Err(Error::Validation(
                "fenced_txn needs at least one statement".to_owned(),
            ));
        }
        let mut batch = Vec::with_capacity(statements.len().saturating_add(1));
        batch.push(Statement::with_params(
            GUARD_SQL,
            vec![
                Value::Integer(fence_literal(lease.token)?),
                Value::from(lease.key.as_str()),
            ],
        ));
        for statement in statements {
            batch.push(statement.fenced(lease.token)?);
        }

        let mut results = self.txn(batch).await.map_err(|e| {
            // The guard is the only statement in the batch that can fail on
            // `lease_fence`, so its failure is the one that means superseded;
            // an error from the caller's own statements passes through with
            // its own class.
            if !e.message().contains(FENCE_COLUMN) {
                return e;
            }
            lease.superseded.store(true, Ordering::Relaxed);
            Error::Conflict(format!(
                "lease {} token {} has been superseded: {}",
                lease.key,
                lease.token.get(),
                e.message()
            ))
        })?;
        if results.is_empty() {
            return Err(Error::Integrity(
                "fenced_txn ran the guard but got no result for it".to_owned(),
            ));
        }
        let guard = results.remove(0);
        if guard.rows_affected != 1 {
            return Err(Error::Conflict(format!(
                "lease {} has no fencing row; the lease was never minted or was cleared",
                lease.key
            )));
        }
        Ok(results)
    }
}

impl Statement {
    /// Rewrite this statement to carry the fencing token (spec 012 B-2).
    ///
    /// `AND fence <= <token>` is appended to the statement's own `WHERE` (a
    /// subquery's `WHERE` is left alone), and an `UPDATE` also sets
    /// `fence = <token>` so the row records the lease that last wrote it. The
    /// token is written as an integer literal, not a parameter, so the
    /// caller's positional parameters keep their numbers.
    ///
    /// The target table needs an integer `fence` column; a statement without
    /// one fails at execution rather than silently writing unguarded.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the statement is not an `UPDATE` or a
    /// `DELETE`. Nothing else can be fenced: an `INSERT` has no row to
    /// compare a token against, so a caller under a lease inserts through
    /// [`StoreHandle::txn`] and stamps `fence` with the lease's token itself.
    pub fn fenced(self, token: FenceToken) -> Result<Self, Error> {
        let literal = fence_literal(token)?;
        let sql = self.sql.trim().trim_end_matches(';').trim_end();
        let verb = sql
            .split_whitespace()
            .next()
            .ok_or_else(|| Error::Validation("fenced needs a statement".to_owned()))?;
        let is_update = verb.eq_ignore_ascii_case("update");
        if !is_update && !verb.eq_ignore_ascii_case("delete") {
            return Err(Error::Validation(format!(
                "only UPDATE and DELETE can be fenced, not {verb}"
            )));
        }

        let sql = match top_level_where(sql) {
            Some(at) => {
                let (head, tail) = split_at(sql, at)?;
                if is_update {
                    format!(
                        "{}, fence = {literal} {tail} AND fence <= {literal}",
                        head.trim_end()
                    )
                } else {
                    format!("{head}{tail} AND fence <= {literal}")
                }
            }
            None => {
                if is_update {
                    format!("{sql}, fence = {literal} WHERE fence <= {literal}")
                } else {
                    format!("{sql} WHERE fence <= {literal}")
                }
            }
        };
        Ok(Self {
            sql,
            params: self.params,
        })
    }
}

/// A token as the `i64` SQLite stores.
fn fence_literal(token: FenceToken) -> Result<i64, Error> {
    i64::try_from(token.get())
        .map_err(|_| Error::Integrity(format!("fencing token {} exceeds i64", token.get())))
}

fn split_at(sql: &str, at: usize) -> Result<(&str, &str), Error> {
    match (sql.get(..at), sql.get(at..)) {
        (Some(head), Some(tail)) => Ok((head, tail)),
        _ => Err(Error::Validation(format!(
            "statement does not split on a character boundary at {at}"
        ))),
    }
}

/// The byte offset of the statement's own `WHERE`, if it has one.
///
/// Only a `WHERE` outside every parenthesis, string literal, quoted
/// identifier, and comment counts, so a subquery's `WHERE` is never mistaken
/// for the statement's.
fn top_level_where(sql: &str) -> Option<usize> {
    let mut chars = sql.char_indices().peekable();
    let mut depth: u32 = 0;
    let mut word: Option<usize> = None;

    while let Some((i, c)) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            if word.is_none() {
                word = Some(i);
            }
            continue;
        }
        if let Some(found) = close_word(sql, word.take(), i, depth) {
            return Some(found);
        }
        match c {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            '\'' | '"' | '`' => skip_quoted(&mut chars, c),
            '[' => skip_quoted(&mut chars, ']'),
            '-' if chars.peek().is_some_and(|(_, n)| *n == '-') => {
                for (_, n) in chars.by_ref() {
                    if n == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek().is_some_and(|(_, n)| *n == '*') => {
                chars.next();
                while let Some((_, n)) = chars.next() {
                    if n == '*' && chars.peek().is_some_and(|(_, m)| *m == '/') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    close_word(sql, word, sql.len(), depth)
}

fn close_word(sql: &str, start: Option<usize>, end: usize, depth: u32) -> Option<usize> {
    let start = start?;
    if depth != 0 {
        return None;
    }
    sql.get(start..end)
        .filter(|w| w.eq_ignore_ascii_case("where"))
        .map(|_| start)
}

fn skip_quoted(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, closer: char) {
    while let Some((_, c)) = chars.next() {
        if c != closer {
            continue;
        }
        // A doubled quote is an escaped one: keep going.
        if chars.peek().is_some_and(|(_, n)| *n == closer) {
            chars.next();
            continue;
        }
        return;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_subquerys_where_is_not_the_statements_own() {
        let sql = "UPDATE t SET v = (SELECT v FROM u WHERE u.id = $1) WHERE t.id = $2";
        let at = top_level_where(sql).expect("the outer WHERE");
        assert_eq!(sql.get(at..), Some("WHERE t.id = $2"));
    }

    #[test]
    fn a_where_inside_a_literal_is_not_a_keyword() {
        let sql = "DELETE FROM t WHERE v = 'where'";
        let at = top_level_where(sql).expect("the only WHERE");
        assert_eq!(at, 14);
    }

    #[test]
    fn an_update_gains_the_column_and_the_predicate() {
        let stmt = Statement::with_params("UPDATE t SET v = $1 WHERE id = $2", vec![])
            .fenced(FenceToken::new(7))
            .expect("an UPDATE can be fenced");
        assert_eq!(
            stmt.sql,
            "UPDATE t SET v = $1, fence = 7 WHERE id = $2 AND fence <= 7"
        );
    }

    #[test]
    fn a_delete_gains_only_the_predicate() {
        let stmt = Statement::new("DELETE FROM t WHERE id = $1;")
            .fenced(FenceToken::new(3))
            .expect("a DELETE can be fenced");
        assert_eq!(stmt.sql, "DELETE FROM t WHERE id = $1 AND fence <= 3");
    }

    #[test]
    fn a_statement_without_a_where_gains_one() {
        let stmt = Statement::new("UPDATE t SET v = 1")
            .fenced(FenceToken::new(2))
            .expect("an UPDATE can be fenced");
        assert_eq!(stmt.sql, "UPDATE t SET v = 1, fence = 2 WHERE fence <= 2");
    }

    #[test]
    fn an_insert_cannot_be_fenced() {
        let err = Statement::new("INSERT INTO t (v) VALUES ($1)")
            .fenced(FenceToken::new(1))
            .expect_err("an INSERT has no row to fence");
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }
}
