//! The `/binding` route (spec 040 B-6), and the OTel resource a binding
//! document implies (B-10).
//!
//! The edge serves bytes the composer hands it and names no ops type, so it
//! keeps its dependency direction (thesis section 4): the document is
//! assembled once at boot by `rahi_ops::binding`, and this module only
//! answers it and reads its present values off the JSON.

use axum::Router;
use axum::body::Bytes;
use axum::http::header;
use axum::response::IntoResponse as _;
use axum::routing::get;
use serde_json::Value;

/// Where the document is served, beside the probes and `/metrics`.
pub const BINDING_PATH: &str = "/binding";

/// The route: `GET /binding` answers `bytes`, unchanged, for the life of the
/// process (B-6). Mounted on the unguarded branch by the edge builder.
pub fn router(bytes: Bytes) -> Router {
    Router::new().route(
        BINDING_PATH,
        get(move || {
            let bytes = bytes.clone();
            async move { ([(header::CONTENT_TYPE, "application/json")], bytes).into_response() }
        }),
    )
}

/// The OTel resource keys B-10 names, each with the document path it is
/// read from. `service.name` is not here: the tracer always carries it.
pub const RESOURCE_KEYS: [(&str, &[&str]); 9] = [
    ("service.version", &["manifest", "contract_version"]),
    ("service.instance.id", &["instance", "id"]),
    ("rahi.version", &["build", "rahi_version"]),
    ("rahi.revision", &["build", "revision"]),
    ("rahi.manifest.hash", &["manifest", "hash"]),
    ("rahi.binary.sha256", &["build", "binary", "sha256"]),
    ("rahi.binary.platform", &["build", "binary", "platform"]),
    ("rahi.node", &["instance", "node"]),
    ("rahi.artifact.image", &["artifact", "image"]),
];

/// The resource attributes `document` implies (B-10): one per
/// [`RESOURCE_KEYS`] entry whose wrapper carries a value, rendered as a
/// string; an absent value is not emitted, and nothing else is, so neither
/// an epoch nor a comparison result reaches the resource.
#[must_use]
pub fn resource_attributes(document: &Value) -> Vec<(String, String)> {
    RESOURCE_KEYS
        .iter()
        .filter_map(|(key, path)| {
            let wrapper = path
                .iter()
                .try_fold(document, |node, step| node.get(step))?;
            let value = match wrapper.get("value")? {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                _ => return None,
            };
            Some(((*key).to_owned(), value))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_absent_value_is_not_emitted_and_a_present_one_is() {
        let document = json!({
            "instance": {
                "id": { "value": "1-00000000000000000000000000000000", "basis": "minted" },
                "node": { "value": 1, "basis": "declared" }
            },
            "build": { "revision": { "basis": "absent", "reason": "not_declared" } },
            "epoch": { "match": { "value": "agrees", "basis": "measured" } }
        });
        let attributes = resource_attributes(&document);
        assert_eq!(
            attributes,
            vec![
                (
                    "service.instance.id".to_owned(),
                    "1-00000000000000000000000000000000".to_owned()
                ),
                ("rahi.node".to_owned(), "1".to_owned()),
            ]
        );
    }
}
