//! The static slot: the app's built SPA, served from one directory
//! (spec 020 B-7).
//!
//! Two caching answers and one routing rule. A hashed asset is immutable and
//! is cached for a year, because its name changes when its bytes do.
//! Everything else revalidates, because it might not. An unknown path under
//! the slot falls back to `index.html`, which is what makes client-side
//! routing work on a hard refresh.

use std::path::Path;

use axum::Router;
use axum::extract::Request;
use axum::http::{HeaderValue, header};
use axum::middleware::{Next, from_fn};
use axum::response::Response;
use tower_http::services::{ServeDir, ServeFile};

/// The document the slot falls back to for an unknown path.
pub const INDEX_FILE: &str = "index.html";
/// What a hashed asset is cached for: a year, and never revalidated.
pub const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// What everything else is cached for: it may be held, but it is revalidated
/// before it is used.
pub const REVALIDATE_CACHE_CONTROL: &str = "no-cache";
/// How long a name segment must be before it can be a content hash.
pub const MIN_HASH_LEN: usize = 8;

/// The slot as a router whose fallback is the directory, and whose fallback
/// in turn is `index.html`.
pub fn service(dir: &Path) -> Router {
    let serve = ServeDir::new(dir)
        .append_index_html_on_directories(true)
        .fallback(ServeFile::new(dir.join(INDEX_FILE)));
    Router::new()
        .fallback_service(serve)
        .layer(from_fn(cache_control))
}

/// Whether the last segment of `path` names a content-hashed asset.
///
/// The rule is deliberately narrow: the stem must carry a `.` or `-`
/// separated tail of at least [`MIN_HASH_LEN`] alphanumeric characters with a
/// digit among them, which admits `index-BsX9k2Lp.js` and `main.4f3a2b1c.js`
/// and refuses `vendor-bootstrap.css`. A false positive pins a stale asset in
/// every cache for a year; a false negative costs one revalidation, so the
/// rule errs toward revalidating (spec 020 D-4).
#[must_use]
pub fn is_hashed_asset(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.eq_ignore_ascii_case(INDEX_FILE) {
        return false;
    }
    let Some((stem, _extension)) = name.rsplit_once('.') else {
        return false;
    };
    let Some((_head, tail)) = stem.rsplit_once(['.', '-']) else {
        return false;
    };
    tail.len() >= MIN_HASH_LEN
        && tail.chars().all(|c| c.is_ascii_alphanumeric())
        && tail.chars().any(|c| c.is_ascii_digit())
}

/// Set the one cache answer the path earns.
async fn cache_control(request: Request, next: Next) -> Response {
    let hashed = is_hashed_asset(request.uri().path());
    let mut response = next.run(request).await;
    let value = if hashed {
        IMMUTABLE_CACHE_CONTROL
    } else {
        REVALIDATE_CACHE_CONTROL
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hashed_asset_is_immutable() {
        assert!(is_hashed_asset("/assets/index-BsX9k2Lp.js"));
        assert!(is_hashed_asset("/assets/main.4f3a2b1c.css"));
        assert!(is_hashed_asset("/app.0123456789abcdef.js"));
    }

    #[test]
    fn everything_else_revalidates() {
        assert!(!is_hashed_asset("/index.html"));
        assert!(!is_hashed_asset("/"));
        assert!(!is_hashed_asset("/app.js"));
        assert!(!is_hashed_asset("/favicon.ico"));
        assert!(!is_hashed_asset("/assets/vendor-bootstrap.css"));
        assert!(!is_hashed_asset("/assets/style-print.css"));
        assert!(!is_hashed_asset("/application.js"));
    }
}
