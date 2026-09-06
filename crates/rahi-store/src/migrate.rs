//! Versioned DDL, applied by the `migrate` verb and never at boot (spec 011
//! B-4, constitution IX).

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
    name TEXT NOT NULL)";

const RECORD_SQL: &str = "INSERT INTO schema_version (version, name) VALUES ($1, $2)";

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
}

impl Migration {
    /// Build a migration.
    #[must_use]
    pub fn new(version: u32, name: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            version,
            name: name.into(),
            sql: sql.into(),
        }
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
struct VersionRow {
    version: i64,
}

impl StoreHandle {
    /// Apply every migration above the recorded version, in order.
    ///
    /// Reads `schema_version` through the leader, applies each higher
    /// migration through one `txn` that also records it, and reports what
    /// changed. Idempotent: a second run with the same list applies
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the list is not strictly ascending or
    /// uses version `0`; [`Error::Conflict`] when the list names a version
    /// below the recorded one that the store has not applied (the list and
    /// the store disagree about history); the statement's error when DDL
    /// fails, with that migration rolled back.
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

        let recorded: Vec<VersionRow> = self
            .query_consistent(
                "SELECT version FROM schema_version ORDER BY version",
                vec![],
            )
            .await?;
        let recorded: Vec<u32> = recorded
            .into_iter()
            .map(|r| u32::try_from(r.version))
            .collect::<Result<_, _>>()
            .map_err(|_| Error::Integrity("schema_version holds a negative version".to_owned()))?;
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
                vec![Value::from(m.version), Value::from(m.name.as_str())],
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
