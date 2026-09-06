//! Binary values and the extension policy (spec 016).
//!
//! Two answers the store owed the applications above it. The first is that
//! bytes are ordinary: a `Vec<u8>` parameter is a BLOB, a BLOB column
//! deserializes into `Vec<u8>`, [`Blob`], or `serde_bytes::ByteBuf`, and no
//! base64 hop exists anywhere in the path. The second is that a value is not
//! free. It is written to the Raft log, shipped to every follower, and
//! retained in the log until the next snapshot, so a single parameter above
//! [`MAX_VALUE_BYTES`] is refused before the statement is submitted rather
//! than paid for N times plus the log.
//!
//! The third answer is a refusal. hiqlite replicates the statement, not the
//! page, so every node executes it against its own engine. A node carrying a
//! virtual table module another node lacks either fails its apply or, worse,
//! succeeds differently, and Raft's guarantee (every node applies the same
//! log to the same state) is void. The store therefore never hands a SQLite
//! extension to a connection, never turns that facility on, and exposes no
//! API that could. [`crate::Store::open`] asks its own engine for one at
//! boot and refuses to run if anything comes back, and `tests/blob.rs` greps
//! this crate's own sources for the three call shapes that could turn it on;
//! the boot check's own documentation says exactly what it can and cannot
//! see. There is no distance function and no index type here either:
//! ranking is application logic above the chassis (constitution XIV).
//!
//! Spec 016 B-5 names, in full, what a later spec must establish before any
//! of that changes. Until such a spec exists and ships, an application that
//! needs similarity search reads its vectors through
//! [`StoreHandle::query_paged`](crate::StoreHandle::query_paged) and ranks
//! them in its own process.

use std::fmt;

use rahi_types::Error;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::query::Value;
use crate::store::StoreHandle;
use crate::txn::Statement;

/// The largest a single SQL parameter may be: 1 MiB (B-2).
///
/// The ceiling is a replication budget, not a SQLite limit. Every parameter
/// is part of the statement the leader appends to the Raft log, so a value is
/// paid for once per node plus once in the log until the next snapshot. A
/// deployment may lower its own ceiling with
/// [`StoreHandle::with_max_value_bytes`]; nothing raises it.
pub const MAX_VALUE_BYTES: usize = 1024 * 1024;

/// What the engine's extension surface is, and always is: nothing (B-4).
///
/// The literal `preflight` prints for the extension check (spec 030 B-3).
pub const EXTENSIONS: &str = "none";

/// The default page of a sweep, in rows (B-3).
pub const DEFAULT_PAGE_ROWS: u32 = 1024;

/// The two engine facts an operator is owed at `preflight` (B-4, AC-2).
///
/// `extensions` is a constant because the answer is a property of the
/// chassis rather than of a deployment; `max_value_bytes` is this handle's
/// ceiling, which a deployment may have lowered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineReport {
    /// Always [`EXTENSIONS`]: no loadable extension is ever present.
    pub extensions: &'static str,
    /// The size a single parameter is refused above, in bytes.
    pub max_value_bytes: usize,
}

impl fmt::Display for EngineReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "extensions: {}, max_value_bytes: {}",
            self.extensions, self.max_value_bytes
        )
    }
}

/// A binary value the store has already agreed to carry (B-1, B-2).
///
/// [`Blob::new`] applies [`MAX_VALUE_BYTES`] once, at the boundary where the
/// bytes are produced, so a caller that builds its values as `Blob`s learns
/// about an oversized vector where it can still do something about it rather
/// than at the write. The write path checks again against the handle's own
/// ceiling: `Blob` is the front door, not the enforcement.
///
/// It deserializes from either read path. A local `query` hands serde the
/// BLOB as bytes; a `query_consistent` comes back from the leader as a
/// sequence of byte-sized integers. Both land in the same `Blob`, and a read
/// is never refused for size: a value written under a higher ceiling still
/// reads back whole.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct Blob(Vec<u8>);

impl fmt::Debug for Blob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blob").field("len", &self.0.len()).finish()
    }
}

impl Blob {
    /// Take ownership of `bytes` if they are within [`MAX_VALUE_BYTES`].
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when the value is larger than the ceiling.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, Error> {
        let bytes = bytes.into();
        if bytes.len() > MAX_VALUE_BYTES {
            return Err(oversized("value", bytes.len(), MAX_VALUE_BYTES));
        }
        Ok(Self(bytes))
    }

    /// The bytes, borrowed.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// The bytes, owned.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// How many bytes this value carries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether this value carries no bytes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[u8]> for Blob {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<Vec<u8>> for Blob {
    type Error = Error;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Error> {
        Self::new(bytes)
    }
}

impl From<Blob> for Value {
    fn from(blob: Blob) -> Self {
        Self::Blob(blob.0)
    }
}

impl Serialize for Blob {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for Blob {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_byte_buf(BlobVisitor)
    }
}

struct BlobVisitor;

impl<'de> Visitor<'de> for BlobVisitor {
    type Value = Blob;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a BLOB column")
    }

    fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Blob, E> {
        Ok(Blob(bytes.to_vec()))
    }

    fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Blob, E> {
        Ok(Blob(bytes))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Blob, A::Error> {
        let mut bytes = Vec::with_capacity(seq.size_hint().unwrap_or(0));
        while let Some(byte) = seq.next_element::<u8>()? {
            bytes.push(byte);
        }
        Ok(Blob(bytes))
    }
}

impl StoreHandle {
    /// The size this handle refuses a single parameter above (B-2).
    #[must_use]
    pub fn max_value_bytes(&self) -> usize {
        self.max_value_bytes
    }

    /// A clone of this handle with a lower ceiling.
    ///
    /// Downward only: the ceiling is a replication budget the chassis sets,
    /// and a subsystem that knows its values are small may hold itself to
    /// less, but nothing buys itself more room than [`MAX_VALUE_BYTES`].
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `bytes` is above this handle's current
    /// ceiling, or is zero.
    pub fn with_max_value_bytes(&self, bytes: usize) -> Result<Self, Error> {
        if bytes == 0 {
            return Err(Error::Validation(
                "max_value_bytes of 0 refuses every parameter".to_owned(),
            ));
        }
        if bytes > self.max_value_bytes {
            return Err(Error::Validation(format!(
                "max_value_bytes {bytes} is above this handle's ceiling of {}; \
                 the ceiling is configurable downward only",
                self.max_value_bytes
            )));
        }
        let mut lowered = self.clone();
        lowered.max_value_bytes = bytes;
        Ok(lowered)
    }

    /// The two engine facts `preflight` reports (AC-2).
    #[must_use]
    pub fn engine_report(&self) -> EngineReport {
        EngineReport {
            extensions: EXTENSIONS,
            max_value_bytes: self.max_value_bytes,
        }
    }
}

/// Refuse a parameter above `ceiling` before the statement is submitted.
pub(crate) fn check_values(values: &[Value], ceiling: usize) -> Result<(), Error> {
    for (i, value) in values.iter().enumerate() {
        let size = match value {
            Value::Blob(bytes) => bytes.len(),
            Value::Text(text) => text.len(),
            Value::Null | Value::Integer(_) | Value::Real(_) => 0,
        };
        if size > ceiling {
            return Err(oversized(&format!("parameter ${}", i + 1), size, ceiling));
        }
    }
    Ok(())
}

/// The same refusal across a batch, before any of it is submitted.
pub(crate) fn check_statements(statements: &[Statement], ceiling: usize) -> Result<(), Error> {
    for (i, statement) in statements.iter().enumerate() {
        check_values(&statement.params, ceiling)
            .map_err(|e| Error::Validation(format!("statement {} of the batch: {e}", i + 1)))?;
    }
    Ok(())
}

fn oversized(what: &str, size: usize, ceiling: usize) -> Error {
    Error::Validation(format!(
        "{what} is {size} bytes, over the {ceiling}-byte ceiling; every parameter is written to \
         the Raft log, shipped to every follower, and kept until the next snapshot"
    ))
}

/// The name of the SQL function that would pull an extension into the
/// engine, assembled from two halves.
///
/// The token never appears in this crate's sources, which is what lets the
/// grep of B-4 be absolute: any occurrence of it, of the connection call that
/// turns the facility on, or of the auto-registration hook is a defect, with
/// no exemption list to argue about.
const PROBE_FN: &str = concat!("load", "_extension");

/// The extension this probe asks for and must never get.
const PROBE_TARGET: &str = "rahi-no-such-extension";

#[derive(Deserialize)]
struct ProbeRow {
    #[allow(dead_code)]
    probe: Option<String>,
}

/// Assert at boot that this node's engine loads no extension (B-4, D-2).
///
/// Asks the local replica to load one and requires that nothing comes back.
/// The request is a read, so it is never appended to the Raft log and never
/// reaches a follower: the assertion is about this node's own engine, which
/// is the node whose divergence would be silent.
///
/// What the answer can say is bounded by hiqlite 0.14, which owns the
/// connection, exposes no handle to it, and drops row-level errors on every
/// local read (`while let Some(Ok(row))`). A refused load and a load that
/// failed for any other reason both arrive here as no rows, so the flag
/// itself is not readable. What is readable is the outcome that matters: a
/// load that succeeded returns exactly one row, and that row is what this
/// function refuses to see. The rest of the ban is static: the crate calls
/// nothing that could turn loading on (the grep of `tests/blob.rs`), and
/// hiqlite calls nothing either.
///
/// # Errors
///
/// [`Error::Config`] when the engine answers: this process is running a
/// SQLite that can be handed a virtual table module the other nodes lack,
/// and one identical engine per cluster is the property the store's
/// replication rests on.
pub(crate) async fn assert_extensions_refused(store: &StoreHandle) -> Result<(), Error> {
    let sql = format!("SELECT {PROBE_FN}($1) AS probe");
    let answered = store
        .query::<ProbeRow>(sql, vec![Value::from(PROBE_TARGET)])
        .await
        .is_ok_and(|rows| !rows.is_empty());
    if answered {
        return Err(Error::Config(format!(
            "this SQLite loaded the extension {PROBE_TARGET:?}; the chassis reports \
             {EXTENSIONS} and hiqlite replicates statements, so an engine that can be extended \
             on one node diverges silently from the others (spec 016 B-4)"
        )));
    }
    Ok(())
}
