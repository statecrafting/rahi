//! Versioned DDL, applied by the `migrate` verb and never at boot (spec 011
//! B-4, constitution IX).
//!
//! Spec 036 B-7 and B-8 add two facts to every recorded version, because a
//! version and a name were not enough to say what had run. The `checksum` is
//! sha256 over the migration's SQL, so a migration edited under a version
//! that is already applied is refused rather than skipped as applied. The
//! `additive` flag is the migration's own declaration that it only creates
//! tables, indexes, or nullable or defaulted columns, which is what lets an
//! older binary serve a store ahead of it (spec 030 B-2 as 036 B-8 extends
//! it). Neither is inferred: an undeclared migration is not additive, and a
//! row with no checksum is one written before this spec.

use attest_ledger_core::sha256_hex;
use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// The store's own baseline: the table that records every migration.
/// Version `0`, applied by [`StoreHandle::migrate`] before anything else.
pub const BASELINE_VERSION: u32 = 0;

// No timestamp column: hiqlite refuses non-deterministic SQL functions on
// the write path (every follower must apply identical bytes), and this
// crate reads no clock. A caller that wants wall-clock provenance ledgers it.
const BASELINE_SQL: &str = "CREATE TABLE IF NOT EXISTS schema_version (\
    version INTEGER PRIMARY KEY, \
    name TEXT NOT NULL, \
    checksum TEXT NULL, \
    additive INTEGER NULL)";

const RECORD_SQL: &str = "INSERT INTO schema_version (version, name, checksum, additive) \
     VALUES ($1, $2, $3, $4)";

/// Which columns `schema_version` already has.
///
/// `CREATE TABLE IF NOT EXISTS` leaves a table that exists exactly as it
/// was, and SQLite has no `ADD COLUMN IF NOT EXISTS`, so the two columns
/// spec 036 B-7 and B-8 add are applied to an existing store here.
const COLUMNS_SQL: &str = "SELECT name FROM pragma_table_info('schema_version')";

/// The columns spec 036 adds, and the statement that adds each.
const ADDED_COLUMNS: [(&str, &str); 2] = [
    (
        "checksum",
        "ALTER TABLE schema_version ADD COLUMN checksum TEXT NULL",
    ),
    (
        "additive",
        "ALTER TABLE schema_version ADD COLUMN additive INTEGER NULL",
    ),
];

/// Recording the binary's checksum on a row written before spec 036 (B-7).
const BACKFILL_SQL: &str =
    "UPDATE schema_version SET checksum = $1, additive = $2 WHERE version = $3";

/// Every recorded version, with what spec 036 records beside it.
const RECORDED_SQL: &str =
    "SELECT version, name, checksum, additive FROM schema_version ORDER BY version";

/// One migration: a version, a name, and its DDL.
///
/// `sql` may hold several statements separated by `;`. Each is applied in
/// order inside the same `txn` that records the version, so a migration
/// either lands whole or not at all. Statement bodies that themselves
/// contain `;` (triggers) are not supported by the splitter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Migration {
    /// Strictly increasing across the list; `0` is reserved for the baseline.
    pub version: u32,
    /// A short name, recorded alongside the version.
    pub name: String,
    /// The DDL.
    pub sql: String,
    /// Whether this migration only creates tables, indexes, or nullable or
    /// defaulted columns (spec 036 B-8).
    ///
    /// `false` unless [`Migration::additive`] declared it, because a
    /// migration that has not said it is safe to serve from an older binary
    /// has not said it: absence is never permission.
    #[serde(default)]
    pub additive: bool,
}

impl Migration {
    /// Build a migration. Not additive until [`Migration::additive`] says so.
    #[must_use]
    pub fn new(version: u32, name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            version,
            name: name.into(),
            sql: sql.into(),
            additive: false,
        }
    }

    /// Declare that this migration only creates tables, indexes, or nullable
    /// or defaulted columns (spec 036 B-8).
    ///
    /// The declaration is recorded with the version, and it is what lets a
    /// binary whose last migration is older than the store serve it: a
    /// rollback across a non-additive migration is refused instead, and the
    /// repair is a forward fix.
    #[must_use]
    pub fn additive(mut self) -> Self {
        self.additive = true;
        self
    }

    /// sha256 over this migration's SQL, hex (spec 036 B-7).
    ///
    /// Over the SQL exactly as declared: a migration whose text is edited
    /// under a version that is already applied is a different migration, and
    /// this is what says so.
    #[must_use]
    pub fn checksum(&self) -> String {
        sha256_hex(self.sql.as_bytes())
    }

    fn statements(&self) -> Vec<Statement> {
        self.sql
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Statement::new)
            .collect()
    }
}

/// What a `migrate` run did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationReport {
    /// The recorded version before the run.
    pub previous: u32,
    /// The recorded version after the run.
    pub current: u32,
    /// The versions this run applied, in order.
    pub applied: Vec<u32>,
}

#[derive(Deserialize)]
struct ColumnRow {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RecordedRow {
    version: i64,
    name: String,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    additive: Option<i64>,
}

/// One row of `schema_version`, as spec 036 B-7 and B-8 leave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedMigration {
    /// The applied version.
    pub version: u32,
    /// The name recorded with it.
    pub name: String,
    /// sha256 over the SQL that was applied, or `None` on a row written
    /// before spec 036 (B-7).
    pub checksum: Option<String>,
    /// Whether the migration declared itself additive, or `None` on a row
    /// written before spec 036 (B-8). `None` is not additive: a version this
    /// store cannot vouch for is not one an older binary may serve across.
    pub additive: Option<bool>,
}

impl RecordedMigration {
    /// Whether an older binary may serve a store that has applied this
    /// version (spec 036 B-8).
    #[must_use]
    pub fn is_additive(&self) -> bool {
        self.additive == Some(true)
    }
}

impl StoreHandle {
    /// Apply every migration above the recorded version, in order.
    ///
    /// Reads `schema_version` through the leader, applies each higher
    /// migration through one `txn` that also records it, and reports what
    /// changed. Idempotent: a second run with the same list applies
    /// nothing.
    ///
    /// Spec 036 B-7 adds one refusal and one repair before anything is
    /// applied: a recorded version whose checksum differs from the binary's
    /// is [`Error::Integrity`], and a row written before that spec, which
    /// carries no checksum, has the binary's recorded onto it now so the
    /// check has something to compare from the next run onward.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the list is not strictly ascending or
    /// uses version `0`; [`Error::Integrity`] when a recorded version's
    /// checksum differs from the binary's (spec 036 B-7);
    /// [`Error::Conflict`] when the list names a version below the recorded
    /// one that the store has not applied (the list and the store disagree
    /// about history); the statement's error when DDL fails, with that
    /// migration rolled back.
    pub async fn migrate(&self, migrations: &[Migration]) -> Result<MigrationReport, Error> {
        validate_list(migrations)?;
        self.txn(vec![
            Statement::new(BASELINE_SQL),
            Statement::with_params(
                "INSERT OR IGNORE INTO schema_version (version, name) VALUES ($1, $2)",
                vec![Value::from(BASELINE_VERSION), Value::from("baseline")],
            ),
        ])
        .await?;
        self.add_migration_columns().await?;

        let history = self.recorded_migrations().await?;
        // Spec 036 B-7: what already ran is checked against what this binary
        // carries before anything new is applied, so a migration edited under
        // an applied version stops the deploy rather than being skipped.
        check_checksums(&history, migrations)?;
        self.record_missing_checksums(&history, migrations).await?;

        let recorded: Vec<u32> = history.iter().map(|row| row.version).collect();
        let previous = recorded.last().copied().unwrap_or(BASELINE_VERSION);

        let mut applied = Vec::new();
        let mut current = previous;
        for m in migrations {
            if recorded.contains(&m.version) {
                continue;
            }
            if m.version < previous {
                return Err(Error::Conflict(format!(
                    "migration {} ({}) is below the recorded schema version {previous} and was never applied",
                    m.version, m.name
                )));
            }
            let mut statements = m.statements();
            if statements.is_empty() {
                return Err(Error::Validation(format!(
                    "migration {} ({}) has no statements",
                    m.version, m.name
                )));
            }
            statements.push(Statement::with_params(
                RECORD_SQL,
                vec![
                    Value::from(m.version),
                    Value::from(m.name.as_str()),
                    Value::from(m.checksum()),
                    Value::from(i64::from(m.additive)),
                ],
            ));
            self.txn(statements).await.map_err(|e| {
                Error::Validation(format!(
                    "migration {} ({}) failed and was rolled back: {}",
                    m.version,
                    m.name,
                    e.message()
                ))
            })?;
            applied.push(m.version);
            current = m.version;
        }
        Ok(MigrationReport {
            previous,
            current,
            applied,
        })
    }

    /// Every recorded version, oldest first (spec 036 B-7, B-8).
    ///
    /// The read `serve` and `restore` ask their compatibility questions of.
    /// It creates nothing: a store no migration has touched has no
    /// `schema_version` table and answers with an empty list.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when a row holds a negative version; the store's
    /// own error when the read fails.
    pub async fn recorded_migrations(&self) -> Result<Vec<RecordedMigration>, Error> {
        let tables: Vec<ColumnRow> = self
            .query_consistent(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
                vec![],
            )
            .await?;
        if tables.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<RecordedRow> = self.query_consistent(RECORDED_SQL, vec![]).await?;
        rows.into_iter()
            .map(|row| {
                Ok(RecordedMigration {
                    version: u32::try_from(row.version).map_err(|_| {
                        Error::Integrity("schema_version holds a negative version".to_owned())
                    })?,
                    name: row.name,
                    checksum: row.checksum.filter(|text| !text.is_empty()),
                    additive: row.additive.map(|flag| flag != 0),
                })
            })
            .collect()
    }

    /// Give an existing `schema_version` table spec 036's two columns.
    async fn add_migration_columns(&self) -> Result<(), Error> {
        let columns: Vec<ColumnRow> = self.query_consistent(COLUMNS_SQL, vec![]).await?;
        // The table was created immediately above, so an answer with no
        // columns at all is the read failing rather than a table with none
        // (spec 016 D-2 records that quirk); guessing either way here would
        // either lose the column or repeat the `ALTER`.
        if columns.is_empty() {
            return Err(Error::Integrity(
                "schema_version reports no columns immediately after it was created: the leader \
                 did not answer the schema probe"
                    .to_owned(),
            ));
        }
        for (name, sql) in ADDED_COLUMNS {
            if columns.iter().any(|column| column.name == name) {
                continue;
            }
            self.txn(vec![Statement::new(sql)]).await?;
        }
        Ok(())
    }

    /// Record the binary's checksum on rows written before spec 036 (B-7).
    ///
    /// Only for versions this binary carries: a version above its last is one
    /// it knows nothing about, and inventing a checksum for it would make the
    /// next check pass on a fiction.
    async fn record_missing_checksums(
        &self,
        history: &[RecordedMigration],
        migrations: &[Migration],
    ) -> Result<(), Error> {
        let statements: Vec<Statement> = history
            .iter()
            .filter(|row| row.checksum.is_none())
            .filter_map(|row| migrations.iter().find(|m| m.version == row.version))
            .map(|m| {
                Statement::with_params(
                    BACKFILL_SQL,
                    vec![
                        Value::from(m.checksum()),
                        Value::from(i64::from(m.additive)),
                        Value::from(m.version),
                    ],
                )
            })
            .collect();
        if statements.is_empty() {
            return Ok(());
        }
        self.txn(statements).await?;
        Ok(())
    }
}

/// Every recorded version this binary also carries hashes to what the binary
/// carries (spec 036 B-7).
///
/// A row with no checksum was written before this spec and is not a
/// mismatch: it has nothing to disagree with, and `migrate` records the
/// binary's onto it.
///
/// # Errors
///
/// [`Error::Integrity`] naming the version, the recorded checksum, and the
/// binary's.
pub fn check_checksums(
    history: &[RecordedMigration],
    migrations: &[Migration],
) -> Result<(), Error> {
    for row in history {
        let Some(recorded) = &row.checksum else {
            continue;
        };
        let Some(m) = migrations.iter().find(|m| m.version == row.version) else {
            continue;
        };
        let ours = m.checksum();
        if &ours != recorded {
            return Err(Error::Integrity(format!(
                "migration {} ({}) was applied with checksum {recorded} and this binary carries \
                 {ours}: the SQL of an applied version changed, so what ran and what this build \
                 declares are not the same migration",
                row.version, row.name
            )));
        }
    }
    Ok(())
}

fn validate_list(migrations: &[Migration]) -> Result<(), Error> {
    let mut last = BASELINE_VERSION;
    for m in migrations {
        if m.version == BASELINE_VERSION {
            return Err(Error::Validation(format!(
                "migration {:?} uses version 0, which is the store's baseline",
                m.name
            )));
        }
        if m.version <= last {
            return Err(Error::Validation(format!(
                "migration {} ({}) does not follow {last}: versions must be strictly ascending",
                m.version, m.name
            )));
        }
        last = m.version;
    }
    Ok(())
}
