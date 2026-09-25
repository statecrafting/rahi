//! Named migration sets (spec 046): a linked library brings its own
//! versioned, checksummed migration sequence, and never renumbers into the
//! host's.
//!
//! A migration's identity is `(set, version)` (I-1). The host cell's own
//! migrations are the set `app` and stay recorded in `schema_version`
//! exactly as spec 011 and 036 left it (B-5, B-13); every other set is
//! recorded in `schema_set_version`. [`plan`] is the pure function B-7
//! names: it orders every pending migration of every set so that each runs
//! after the requirements it carries are met, and refuses an unsatisfiable
//! or cyclic plan before anything is applied (B-8, I-4).

use std::collections::{BTreeMap, BTreeSet};

use rahi_types::Error;
use serde::{Deserialize, Serialize};

use crate::migrate::{
    BASELINE_VERSION, Migration, RecordedMigration, check_checksums, validate_list,
};
use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// The host cell's own set (B-2).
pub const APP_SET: &str = "app";

/// The prefix reserved for the chassis's own sets (B-2).
pub const CHASSIS_PREFIX: &str = "rahi.";

const MAX_NAME_BYTES: usize = 64;

/// Created in the baseline step beside `schema_version` (B-4, B-13).
pub(crate) const SET_TABLE_SQL: &str = "CREATE TABLE IF NOT EXISTS schema_set_version (\
    set_name TEXT NOT NULL, \
    version INTEGER NOT NULL, \
    name TEXT NOT NULL, \
    checksum TEXT NOT NULL, \
    additive INTEGER NOT NULL, \
    PRIMARY KEY (set_name, version))";

const SET_RECORD_SQL: &str = "INSERT INTO schema_set_version \
     (set_name, version, name, checksum, additive) VALUES (?1, ?2, ?3, ?4, ?5)";

const SET_RECORDED_SQL: &str = "SELECT set_name, version, name, checksum, additive \
     FROM schema_set_version ORDER BY set_name, version";

/// A set's name (B-2): `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`, at most 64
/// bytes.
///
/// [`SetName::new`] is the constructor a library or a cell uses, and it
/// refuses the two reserved names: `app`, which is the host's own
/// migrations, and the `rahi.` prefix, which only this crate's
/// [`coordination_set`](crate::coordination_set) and
/// [`receipt_set`](crate::receipt_set) carry. A requirement may still name
/// any set, reserved or not, through [`SetName::reference`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SetName(String);

impl SetName {
    /// A library's own set name.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `name` breaks the grammar, exceeds 64
    /// bytes, is `app`, or starts with `rahi.`.
    pub fn new(name: impl Into<String>) -> Result<Self, Error> {
        let name = Self::reference(name)?;
        if name.is_app() {
            return Err(Error::Validation(format!(
                "the set name {APP_SET:?} is the host cell's own migrations; a library cannot use it"
            )));
        }
        if name.is_chassis() {
            return Err(Error::Validation(format!(
                "the set name {:?} uses the prefix {CHASSIS_PREFIX:?}, which is reserved for the \
                 chassis's own sets",
                name.0
            )));
        }
        Ok(name)
    }

    /// Any well-formed set name, reserved ones included: the form a
    /// requirement names another set by (B-6).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `name` breaks the grammar or exceeds 64
    /// bytes.
    pub fn reference(name: impl Into<String>) -> Result<Self, Error> {
        let name = name.into();
        if name.is_empty() || name.len() > MAX_NAME_BYTES {
            return Err(Error::Validation(format!(
                "a set name must be 1 to {MAX_NAME_BYTES} bytes, got {}",
                name.len()
            )));
        }
        for segment in name.split('.') {
            let mut bytes = segment.bytes();
            let well_formed = bytes.next().is_some_and(|b| b.is_ascii_lowercase())
                && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !well_formed {
                return Err(Error::Validation(format!(
                    "the set name {name:?} does not match ^[a-z][a-z0-9-]*(\\.[a-z][a-z0-9-]*)*$"
                )));
            }
        }
        Ok(Self(name))
    }

    /// The host cell's set.
    #[must_use]
    pub fn app() -> Self {
        Self(APP_SET.to_owned())
    }

    /// A chassis set, named only inside this crate.
    pub(crate) fn chassis(suffix: &str) -> Self {
        Self(format!("{CHASSIS_PREFIX}{suffix}"))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this is the set `app`.
    #[must_use]
    pub fn is_app(&self) -> bool {
        self.0 == APP_SET
    }

    /// Whether this is a chassis set (`rahi.` prefix).
    #[must_use]
    pub fn is_chassis(&self) -> bool {
        self.0.starts_with(CHASSIS_PREFIX)
    }

    /// B-7's ready-together rank: chassis sets, then libraries, then `app`.
    fn rank(&self) -> u8 {
        if self.is_chassis() {
            0
        } else if self.is_app() {
            2
        } else {
            1
        }
    }
}

impl std::fmt::Display for SetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// This set, or this migration and every later one in its set, needs the
/// named set at or above `min_version` (B-6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetRequirement {
    /// The set required.
    pub set: SetName,
    /// The lowest version of it that satisfies the requirement.
    pub min_version: u32,
}

impl SetRequirement {
    /// A requirement on `set` at or above `min_version`.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `set` is not a well-formed set name.
    pub fn new(set: &str, min_version: u32) -> Result<Self, Error> {
        Ok(Self {
            set: SetName::reference(set)?,
            min_version,
        })
    }
}

/// A named, versioned migration sequence (B-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationSet {
    /// The set's name.
    pub name: SetName,
    /// Its migrations, strictly ascending from 1.
    pub migrations: Vec<Migration>,
    /// What every migration of this set needs.
    pub requires: Vec<SetRequirement>,
}

impl MigrationSet {
    /// A library's set.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `name` is not a name a library may use
    /// ([`SetName::new`]).
    pub fn new(name: &str, migrations: Vec<Migration>) -> Result<Self, Error> {
        Ok(Self {
            name: SetName::new(name)?,
            migrations,
            requires: Vec::new(),
        })
    }

    pub(crate) fn chassis(suffix: &str, migrations: Vec<Migration>) -> Self {
        Self {
            name: SetName::chassis(suffix),
            migrations,
            requires: Vec::new(),
        }
    }

    /// Declare that every migration of this set needs `set` at or above
    /// `min_version` (B-6).
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `set` is not a well-formed set name.
    pub fn requires(mut self, set: &str, min_version: u32) -> Result<Self, Error> {
        self.requires.push(SetRequirement::new(set, min_version)?);
        Ok(self)
    }

    /// The version this set's last migration carries, or the baseline.
    #[must_use]
    pub fn last_version(&self) -> u32 {
        self.migrations
            .iter()
            .map(|m| m.version)
            .max()
            .unwrap_or(BASELINE_VERSION)
    }

    /// The requirements `migration` carries: the set's own, and every one
    /// declared on it or on an earlier migration of the set (B-6).
    fn requirements_of<'a>(&'a self, migration: &Migration) -> Vec<&'a SetRequirement> {
        self.requires
            .iter()
            .chain(
                self.migrations
                    .iter()
                    .filter(|m| m.version <= migration.version)
                    .flat_map(|m| m.requires.iter()),
            )
            .collect()
    }
}

/// Every set's recorded history, `app` included (B-5).
pub type SetHistories = BTreeMap<SetName, Vec<RecordedMigration>>;

/// One step of a plan: a set and the migration of it to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMigration {
    /// The set.
    pub set: SetName,
    /// The migration.
    pub migration: Migration,
}

/// What a [`StoreHandle::migrate_sets`] run did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMigrationReport {
    /// Every `(set, version)` this run applied, in the order applied.
    pub applied: Vec<(String, u32)>,
    /// Each set's last recorded version after the run.
    pub current: BTreeMap<String, u32>,
}

/// The whole cell as sets: `app` first, then the rest as declared.
fn all_sets(app: &[Migration], sets: &[MigrationSet]) -> Vec<MigrationSet> {
    let mut all = vec![MigrationSet {
        name: SetName::app(),
        migrations: app.to_vec(),
        requires: Vec::new(),
    }];
    all.extend(sets.iter().cloned());
    all
}

/// The cell's sets are well formed (B-1, B-2, B-8): unique names, no
/// library named `app`, ascending versions, every requirement naming a set
/// the cell carries at a version it has, and no cycle among requirements.
///
/// # Errors
///
/// [`Error::Validation`] naming the sets involved.
pub fn validate_sets(sets: &[MigrationSet]) -> Result<(), Error> {
    let mut seen = BTreeSet::new();
    for set in sets {
        if !seen.insert(set.name.clone()) {
            return Err(Error::Validation(format!(
                "the migration set {} is declared twice",
                set.name
            )));
        }
        validate_list(&set.migrations)
            .map_err(|e| Error::Validation(format!("set {}: {}", set.name, e.message())))?;
    }
    let last: BTreeMap<&SetName, u32> = sets
        .iter()
        .map(|set| (&set.name, set.last_version()))
        .collect();
    let mut edges: BTreeMap<&SetName, BTreeSet<&SetName>> = BTreeMap::new();
    for set in sets {
        let requirements = set
            .requires
            .iter()
            .chain(set.migrations.iter().flat_map(|m| m.requires.iter()));
        for req in requirements {
            let Some(&top) = last.get(&req.set) else {
                return Err(Error::Validation(format!(
                    "set {} requires set {} >= {}, which this cell does not carry",
                    set.name, req.set, req.min_version
                )));
            };
            if req.set == set.name {
                return Err(Error::Validation(format!(
                    "set {} requires itself, which is a cycle",
                    set.name
                )));
            }
            if req.min_version > top {
                return Err(Error::Validation(format!(
                    "set {} requires set {} >= {}, and this binary's {} ends at version {top}",
                    set.name, req.set, req.min_version, req.set
                )));
            }
            edges.entry(&set.name).or_default().insert(&req.set);
        }
    }
    if let Some(cycle) = find_cycle(&edges) {
        return Err(Error::Validation(format!(
            "the requirements between migration sets form a cycle: {}",
            cycle
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" -> ")
        )));
    }
    Ok(())
}

/// A cycle in `edges`, as the path that closes it, if there is one.
fn find_cycle<'a>(
    edges: &BTreeMap<&'a SetName, BTreeSet<&'a SetName>>,
) -> Option<Vec<&'a SetName>> {
    fn visit<'a>(
        node: &'a SetName,
        edges: &BTreeMap<&'a SetName, BTreeSet<&'a SetName>>,
        path: &mut Vec<&'a SetName>,
        done: &mut BTreeSet<&'a SetName>,
    ) -> Option<Vec<&'a SetName>> {
        if let Some(at) = path.iter().position(|n| *n == node) {
            let mut cycle: Vec<&SetName> = path.iter().skip(at).copied().collect();
            cycle.push(node);
            return Some(cycle);
        }
        if done.contains(node) {
            return None;
        }
        path.push(node);
        for next in edges.get(node).into_iter().flatten() {
            if let Some(cycle) = visit(next, edges, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        done.insert(node);
        None
    }
    let mut done = BTreeSet::new();
    for node in edges.keys() {
        let mut path = Vec::new();
        if let Some(cycle) = visit(node, edges, &mut path, &mut done) {
            return Some(cycle);
        }
    }
    None
}

/// Every recorded version of every set the binary carries hashes to what the
/// binary carries (B-9). Integrity is checked for every set before any
/// other answer (036 D-13).
///
/// # Errors
///
/// [`Error::Integrity`] naming the set and the version.
pub fn check_set_checksums(histories: &SetHistories, sets: &[MigrationSet]) -> Result<(), Error> {
    for set in sets {
        let Some(history) = histories.get(&set.name) else {
            continue;
        };
        check_checksums(history, &set.migrations)
            .map_err(|e| Error::Integrity(format!("set {}: {}", set.name, e.message())))?;
    }
    Ok(())
}

/// The whole cross-set plan (B-7): every pending migration of every set,
/// each after the requirements it carries are met by a recorded or an
/// earlier planned version, ready-together migrations ordered chassis sets,
/// then libraries by name, then `app`.
///
/// Pure: a function of the sets and the histories only.
///
/// # Errors
///
/// [`Error::Validation`] when [`validate_sets`] refuses the sets or no
/// order satisfies the requirements; [`Error::Conflict`] when a set's
/// history holds a version above a migration the binary carries that the
/// store never applied (the list and the store disagree about history, as
/// 011's `migrate` refuses).
pub fn plan(
    sets: &[MigrationSet],
    histories: &SetHistories,
) -> Result<Vec<PlannedMigration>, Error> {
    validate_sets(sets)?;
    let empty = Vec::new();
    // Per set: the highest version recorded or planned so far, and the
    // queue of pending migrations in version order.
    let mut reached: BTreeMap<&SetName, u32> = BTreeMap::new();
    let mut pending: BTreeMap<&SetName, std::collections::VecDeque<&Migration>> = BTreeMap::new();
    for set in sets {
        let history = histories.get(&set.name).unwrap_or(&empty);
        let recorded: BTreeSet<u32> = history.iter().map(|row| row.version).collect();
        let top = recorded.iter().copied().max().unwrap_or(BASELINE_VERSION);
        let queue: std::collections::VecDeque<&Migration> = set
            .migrations
            .iter()
            .filter(|m| !recorded.contains(&m.version))
            .collect();
        if let Some(below) = queue.iter().find(|m| m.version < top) {
            return Err(Error::Conflict(format!(
                "set {}: migration {} ({}) is below the recorded version {top} and was never \
                 applied",
                set.name, below.version, below.name
            )));
        }
        reached.insert(&set.name, top);
        pending.insert(&set.name, queue);
    }

    let by_name: BTreeMap<&SetName, &MigrationSet> =
        sets.iter().map(|set| (&set.name, set)).collect();
    let mut order: Vec<&SetName> = by_name.keys().copied().collect();
    order.sort_by(|a, b| a.rank().cmp(&b.rank()).then_with(|| a.cmp(b)));

    let mut planned = Vec::new();
    loop {
        let next = order.iter().copied().find(|name| {
            let (Some(set), Some(Some(head))) =
                (by_name.get(name), pending.get(name).map(|q| q.front()))
            else {
                return false;
            };
            set.requirements_of(head)
                .iter()
                .all(|req| reached.get(&req.set).is_some_and(|v| *v >= req.min_version))
        });
        let Some(name) = next else { break };
        let Some(migration) = pending
            .get_mut(name)
            .and_then(std::collections::VecDeque::pop_front)
        else {
            break;
        };
        reached.insert(name, migration.version);
        planned.push(PlannedMigration {
            set: name.clone(),
            migration: migration.clone(),
        });
    }

    let stuck: Vec<String> = pending
        .iter()
        .filter(|(_, queue)| !queue.is_empty())
        .map(|(name, queue)| {
            let head = queue.front().map_or(0, |m| m.version);
            format!("{name} version {head}")
        })
        .collect();
    if !stuck.is_empty() {
        return Err(Error::Validation(format!(
            "no order of the pending migrations satisfies their requirements; blocked: {}",
            stuck.join(", ")
        )));
    }
    Ok(planned)
}

#[derive(Debug, Deserialize)]
struct SetRow {
    set_name: String,
    version: i64,
    name: String,
    checksum: String,
    additive: i64,
}

#[derive(Deserialize)]
struct TableRow {
    #[allow(dead_code)]
    name: String,
}

impl StoreHandle {
    /// Every set's recorded history (B-5): `app` from `schema_version`,
    /// every other set from `schema_set_version`. Creates nothing: a store
    /// with neither table answers with empty histories.
    ///
    /// # Errors
    ///
    /// [`Error::Integrity`] when a row holds a negative version or a set
    /// name outside the grammar; the store's error when the read fails.
    pub async fn recorded_set_migrations(&self) -> Result<SetHistories, Error> {
        let mut histories = SetHistories::new();
        histories.insert(SetName::app(), self.recorded_migrations().await?);
        let tables: Vec<TableRow> = self
            .query_consistent(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name = 'schema_set_version'",
                vec![],
            )
            .await?;
        if tables.is_empty() {
            return Ok(histories);
        }
        let rows: Vec<SetRow> = self.query_consistent(SET_RECORDED_SQL, vec![]).await?;
        for row in rows {
            let set = SetName::reference(row.set_name).map_err(|e| {
                Error::Integrity(format!("schema_set_version holds a bad set name: {e}"))
            })?;
            let version = u32::try_from(row.version).map_err(|_| {
                Error::Integrity(format!(
                    "schema_set_version holds a negative version in {set}"
                ))
            })?;
            histories.entry(set).or_default().push(RecordedMigration {
                version,
                name: row.name,
                checksum: Some(row.checksum),
                additive: Some(row.additive != 0),
            });
        }
        Ok(histories)
    }

    /// Apply every pending migration of `app` and of every named set, in
    /// the order [`plan`] computes (spec 046 B-7, B-8).
    ///
    /// The baseline runs first, as [`StoreHandle::migrate`]'s does, and adds
    /// `schema_set_version`. Then every set's history is read with
    /// `query_consistent`, every set's checksums are checked (B-9), and the
    /// whole plan is computed; a refusal at any of those steps has applied
    /// nothing (I-4). Each migration then applies in its own `txn` with its
    /// record; a failure stops the run with every earlier one applied, and
    /// the next run resumes the plan.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] from [`validate_sets`] or [`plan`];
    /// [`Error::Integrity`] on a checksum mismatch in any set;
    /// [`Error::Conflict`] when a set's history and its list disagree; the
    /// statement's error when DDL fails, with that migration rolled back.
    pub async fn migrate_sets(
        &self,
        app: &[Migration],
        sets: &[MigrationSet],
    ) -> Result<SetMigrationReport, Error> {
        let all = all_sets(app, sets);
        validate_sets(&all)?;
        self.migration_baseline().await?;

        let histories = self.recorded_set_migrations().await?;
        check_set_checksums(&histories, &all)?;
        if let Some(app_history) = histories.get(&SetName::app()) {
            self.record_missing_checksums(app_history, app).await?;
        }
        let steps = plan(&all, &histories)?;

        let mut report = SetMigrationReport {
            applied: Vec::new(),
            current: histories
                .iter()
                .map(|(name, rows)| {
                    (
                        name.to_string(),
                        rows.iter()
                            .map(|r| r.version)
                            .max()
                            .unwrap_or(BASELINE_VERSION),
                    )
                })
                .collect(),
        };
        for step in steps {
            let m = &step.migration;
            let mut statements = m.statements();
            if statements.is_empty() {
                return Err(Error::Validation(format!(
                    "set {}: migration {} ({}) has no statements",
                    step.set, m.version, m.name
                )));
            }
            statements.push(if step.set.is_app() {
                crate::migrate::record_statement(m)
            } else {
                Statement::with_params(
                    SET_RECORD_SQL,
                    vec![
                        Value::from(step.set.as_str()),
                        Value::from(m.version),
                        Value::from(m.name.as_str()),
                        Value::from(m.checksum()),
                        Value::from(i64::from(m.additive)),
                    ],
                )
            });
            self.txn(statements).await.map_err(|e| {
                Error::Validation(format!(
                    "set {}: migration {} ({}) failed and was rolled back: {}",
                    step.set,
                    m.version,
                    m.name,
                    e.message()
                ))
            })?;
            report.applied.push((step.set.to_string(), m.version));
            report.current.insert(step.set.to_string(), m.version);
        }
        Ok(report)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn m(version: u32) -> Migration {
        Migration::new(
            version,
            format!("m{version}"),
            format!("CREATE TABLE t{version} (x)"),
        )
    }

    fn recorded(versions: &[u32]) -> Vec<RecordedMigration> {
        versions
            .iter()
            .map(|v| RecordedMigration {
                version: *v,
                name: format!("m{v}"),
                checksum: Some(m(*v).checksum()),
                additive: Some(false),
            })
            .collect()
    }

    #[test]
    fn names_follow_the_grammar_and_the_reservations() {
        for bad in ["", "Aicortex", "1lib", "lib.", ".lib", "lib..x", "lib_x"] {
            assert!(SetName::new(bad).is_err(), "{bad:?}");
        }
        assert!(SetName::new("a".repeat(65)).is_err());
        assert!(SetName::new("a".repeat(64)).is_ok());
        assert!(SetName::new("app").is_err(), "app is the host's");
        assert!(
            SetName::new("rahi.receipts").is_err(),
            "rahi. is the chassis's"
        );
        assert!(SetName::reference("rahi.receipts").is_ok());
        assert!(SetName::new("aicortex").is_ok());
        assert!(SetName::new("tm.claims-v2").is_ok());
    }

    #[test]
    fn the_plan_orders_chassis_then_libraries_then_app_and_honours_requirements() {
        let receipts = MigrationSet::chassis("receipts", vec![m(1)])
            .requires("rahi.coordination", 1)
            .unwrap();
        let coordination = MigrationSet::chassis("coordination", vec![m(1)]);
        let lib = MigrationSet::new("aicortex", vec![m(1), m(2)])
            .unwrap()
            .requires("rahi.receipts", 1)
            .unwrap();
        let all = all_sets(&[m(1)], &[lib, receipts, coordination]);
        let steps = plan(&all, &SetHistories::new()).unwrap();
        let order: Vec<(String, u32)> = steps
            .iter()
            .map(|s| (s.set.to_string(), s.migration.version))
            .collect();
        assert_eq!(
            order,
            vec![
                ("rahi.coordination".to_owned(), 1),
                ("rahi.receipts".to_owned(), 1),
                ("aicortex".to_owned(), 1),
                ("aicortex".to_owned(), 2),
                ("app".to_owned(), 1),
            ]
        );
    }

    #[test]
    fn a_per_migration_requirement_can_wait_for_a_later_set() {
        // app v2 needs aicortex >= 2; aicortex ranks before app anyway, so
        // make the library require app instead to show the wait.
        let lib = MigrationSet::new("lib", vec![m(1), m(2).requires("app", 2).unwrap()]).unwrap();
        let all = all_sets(&[m(1), m(2)], &[lib]);
        let steps = plan(&all, &SetHistories::new()).unwrap();
        let order: Vec<(String, u32)> = steps
            .iter()
            .map(|s| (s.set.to_string(), s.migration.version))
            .collect();
        assert_eq!(
            order,
            vec![
                ("lib".to_owned(), 1),
                ("app".to_owned(), 1),
                ("app".to_owned(), 2),
                ("lib".to_owned(), 2),
            ]
        );
    }

    #[test]
    fn recorded_versions_are_not_planned_again() {
        let lib = MigrationSet::new("lib", vec![m(1), m(2)]).unwrap();
        let all = all_sets(&[], &[lib]);
        let mut histories = SetHistories::new();
        histories.insert(SetName::reference("lib").unwrap(), recorded(&[1]));
        let steps = plan(&all, &histories).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].migration.version, 2);
    }

    #[test]
    fn an_unsatisfiable_or_cyclic_requirement_is_refused() {
        let receipts = MigrationSet::chassis("receipts", vec![m(1)]);
        let lib = MigrationSet::new("lib", vec![m(1)])
            .unwrap()
            .requires("rahi.receipts", 2)
            .unwrap();
        let err = plan(&all_sets(&[], &[receipts, lib]), &SetHistories::new()).unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
        assert!(err.message().contains("rahi.receipts"), "{err}");

        let a = MigrationSet::new("a", vec![m(1)])
            .unwrap()
            .requires("b", 1)
            .unwrap();
        let b = MigrationSet::new("b", vec![m(1)])
            .unwrap()
            .requires("a", 1)
            .unwrap();
        let err = plan(&all_sets(&[], &[a, b]), &SetHistories::new()).unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
        assert!(err.message().contains("cycle"), "{err}");

        let missing = MigrationSet::new("a", vec![m(1)])
            .unwrap()
            .requires("nope", 1)
            .unwrap();
        let err = plan(&all_sets(&[], &[missing]), &SetHistories::new()).unwrap_err();
        assert!(err.message().contains("nope"), "{err}");
    }

    #[test]
    fn a_duplicate_set_is_refused() {
        let a = MigrationSet::new("a", vec![m(1)]).unwrap();
        let err = validate_sets(&all_sets(&[], &[a.clone(), a])).unwrap_err();
        assert!(matches!(err, Error::Validation(_)), "{err}");
    }
}
