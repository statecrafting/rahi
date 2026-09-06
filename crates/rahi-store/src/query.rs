//! Parameters and row mapping for the two read calls (spec 011 B-3).
//!
//! A local `query` maps rows through serde on the borrowed rusqlite row. A
//! `query_consistent` comes back from the leader as owned rows, which this
//! module turns into a name-to-value map and hands to serde, so both calls
//! deserialize into the same caller types.

use rahi_types::Error;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

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
