//! Parameters and row mapping for the two read calls (spec 011 B-3).
//!
//! A local `query` maps rows through serde on the borrowed rusqlite row. A
//! `query_consistent` comes back from the leader as owned rows, which this
//! module turns into a name-to-value map and hands to serde, so both calls
//! deserialize into the same caller types.
//!
//! Spec 016 adds the third read: [`StoreHandle::query_paged`] is the
//! supported way to sweep a table, because neither of the first two bounds
//! what it returns.

use rahi_types::Error;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::blob::DEFAULT_PAGE_ROWS;
use crate::store::StoreHandle;

/// A SQL parameter value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// A 64-bit integer.
    Integer(i64),
    /// A double.
    Real(f64),
    /// Text.
    Text(String),
    /// A blob.
    Blob(Vec<u8>),
}

impl Value {
    pub(crate) fn into_param(self) -> hiqlite::Param {
        match self {
            Self::Null => hiqlite::Param::Null,
            Self::Integer(i) => hiqlite::Param::Integer(i),
            Self::Real(r) => hiqlite::Param::Real(r),
            Self::Text(t) => hiqlite::Param::Text(t),
            Self::Blob(b) => hiqlite::Param::Blob(b),
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Integer(i64::from(v))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Self::Blob(v)
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Self::Blob(v.to_vec())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

/// Build hiqlite params from values.
pub(crate) fn params(values: Vec<Value>) -> hiqlite::Params {
    values.into_iter().map(Value::into_param).collect()
}

/// Map an owned row from the leader into `T` through serde.
///
/// hiqlite serialises an owned row as `{"columns":[{"name","value"}]}` with
/// the value externally tagged (`{"Integer":1}`, `"Null"`); this flattens it
/// into `{"name": value}` before deserializing.
pub(crate) fn owned_row_to<T: DeserializeOwned>(row: hiqlite::Row<'_>) -> Result<T, Error> {
    let hiqlite::Row::Owned(owned) = row else {
        return Err(Error::Upstream(
            "query_consistent returned a borrowed row; expected an owned one".to_owned(),
        ));
    };
    let raw = serde_json::to_value(&owned).map_err(|e| Error::Integrity(e.to_string()))?;
    let columns = raw
        .get("columns")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Error::Integrity("owned row without columns".to_owned()))?;
    let mut object = serde_json::Map::with_capacity(columns.len());
    for column in columns {
        let name = column
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| Error::Integrity("owned column without a name".to_owned()))?;
        let value = column
            .get("value")
            .ok_or_else(|| Error::Integrity("owned column without a value".to_owned()))?;
        object.insert(name.to_owned(), untag(value));
    }
    serde_json::from_value(serde_json::Value::Object(object))
        .map_err(|e| Error::Validation(format!("row does not fit the requested type: {e}")))
}

fn untag(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) if map.len() == 1 => map
            .values()
            .next()
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        serde_json::Value::String(s) if s == "Null" => serde_json::Value::Null,
        other => other.clone(),
    }
}

/// One page of a sweep (spec 016 B-3).
///
/// A sweep is a loop over [`StoreHandle::query_paged`], never one unbounded
/// `query`, so the peak memory of a full-table scan is a property of
/// [`Page::size`] rather than of how large the table grew.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    /// Rows in this page. Defaults to [`DEFAULT_PAGE_ROWS`].
    pub size: u32,
    /// Rows to skip: the `OFFSET` this page starts after.
    pub after: u64,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            size: DEFAULT_PAGE_ROWS,
            after: 0,
        }
    }
}

impl Page {
    /// The first page of `size` rows.
    #[must_use]
    pub const fn new(size: u32) -> Self {
        Self { size, after: 0 }
    }

    /// The page after this one: the same size, `size` rows further on.
    #[must_use]
    pub const fn next(self) -> Self {
        Self {
            size: self.size,
            after: self.after.saturating_add(self.size as u64),
        }
    }
}

impl StoreHandle {
    /// One page of a read, from the local replica (spec 016 B-3).
    ///
    /// Appends `LIMIT` and `OFFSET` to `sql` as two further positional
    /// parameters, so `sql` numbers its own from `$1` and does not carry a
    /// `LIMIT` of its own. Order is the caller's business: a sweep whose
    /// statement does not order its rows may see a row twice and miss
    /// another, because `OFFSET` counts rows rather than remembering them.
    ///
    /// Reads the local replica, like [`Self::query`]: a sweep is a scan, and
    /// a scan that took a leader round-trip per page would pause Raft once
    /// per page. A sweep that must not miss a concurrent write reads
    /// [`crate::Watermark::since`] afterwards.
    ///
    /// # Errors
    ///
    /// [`Error::Validation`] when `page.size` is zero, when `page.after`
    /// exceeds `i64`, or for a bad statement or a row that does not fit `T`.
    pub async fn query_paged<T>(
        &self,
        sql: &str,
        values: Vec<Value>,
        page: Page,
    ) -> Result<Vec<T>, Error>
    where
        T: DeserializeOwned + Send + 'static,
    {
        if page.size == 0 {
            return Err(Error::Validation(
                "a page of zero rows never advances a sweep".to_owned(),
            ));
        }
        let offset = i64::try_from(page.after)
            .map_err(|_| Error::Validation(format!("page offset {} exceeds i64", page.after)))?;
        // hiqlite binds `$n` by position, so the appended clause names the two
        // numbers after the caller's own rather than a bare `?`, whose index
        // SQLite would derive from the highest one already in the statement.
        let limit = values.len() + 1;
        let mut values = values;
        values.push(Value::Integer(i64::from(page.size)));
        values.push(Value::Integer(offset));
        self.query(
            format!("{sql} LIMIT ${limit} OFFSET ${}", limit + 1),
            values,
        )
        .await
    }
}
