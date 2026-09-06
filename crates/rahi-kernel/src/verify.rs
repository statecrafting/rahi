//! Verify at build: observed usage is inside the declared ceiling
//! (spec 015 B-3).
//!
//! A declared ceiling nobody checks is a comment. The check is a build step:
//! [`verify!`] walks the calling crate's `src/` for the places it reaches a
//! governed facade, and refuses to build when any of them is outside the
//! manifest's grants. The failure names the exact `(service, kind, resource)`
//! that is missing, because the author's next action is to add that grant or
//! delete that call.
//!
//! Two forms name a call site, and the walk reads both as text:
//!
//! - `Governed::new(&kernel, "notes", CapabilityKind::DbWrite, "notes", store)`
//!   is the ordinary one. The triple is right there in the literals, it is
//!   type-checked, and the runtime adjudicates the same triple, so the
//!   declaration and the enforcement cannot drift.
//! - `#[governed(service = "notes", kind = "db.write", resource = "notes")]`
//!   is for a site that builds its triple at runtime, where no literal exists
//!   to read. It is a marker rather than a procedural macro: the kernel has no
//!   proc-macro companion crate, by AC-2, so it is written above the call as a
//!   comment and read from the source text (D-6).
//!
//! Absence is never permission. A `Governed::new` whose triple is not literal
//! and that carries no marker is a build failure, not an unverified call:
//! silence about a call site is exactly the state this step exists to catch.
//!
//! ```no_run
//! # use rahi_kernel::{Manifest, verify};
//! # fn build() -> Result<(), rahi_types::Error> {
//! let manifest = Manifest::parse(include_str!("../testdata/manifests/valid.toml"))?;
//! verify!(&manifest)?; // in build.rs, or in a test
//! # Ok(())
//! # }
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use rahi_types::Error;

use crate::capability::{CapabilityKind, ServiceName};
use crate::manifest::Manifest;

/// How many source lines above an opaque call site a marker may sit.
const MARKER_LOOKBACK_LINES: usize = 5;

/// The marker's text form.
const MARKER: &str = "#[governed(";

/// The constructor the walk recognizes.
const CONSTRUCTOR: &str = "Governed::new(";

/// One observed use of a governed facade.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Usage {
    /// The service the call is attributed to.
    pub service: String,
    /// What it does.
    pub kind: CapabilityKind,
    /// What it does it to.
    pub resource: String,
    /// `path:line`, for the failure message.
    pub site: String,
}

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "({}, {}, {}) at {}",
            self.service, self.kind, self.resource, self.site
        )
    }
}

/// Walk a crate's `src/` and check every governed call site against `manifest`.
///
/// `root` is the crate directory, the one holding `Cargo.toml`. This is what
/// [`verify!`] passes.
///
/// # Errors
///
/// [`Error::Io`] when `root/src` cannot be read; [`Error::Validation`] when a
/// call site cannot be read or is outside the ceiling.
pub fn verify_crate(manifest: &Manifest, root: &Path) -> Result<(), Error> {
    verify_usage(manifest, &scan_crate(root)?)
}

/// Every governed call site under `root/src`, sorted.
///
/// # Errors
///
/// [`Error::Io`] when the tree cannot be read; [`Error::Validation`] when a
/// call site names no readable triple.
pub fn scan_crate(root: &Path) -> Result<Vec<Usage>, Error> {
    let src = root.join("src");
    if !src.is_dir() {
        return Err(Error::Io(format!(
            "{} has no src/ to walk: verify! runs in the crate that composes the chassis",
            root.display()
        )));
    }
    let mut files = Vec::new();
    collect_rust_files(&src, &mut files)?;
    files.sort();

    let mut usage = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file)
            .map_err(|e| Error::Io(format!("{} cannot be read: {e}", file.display())))?;
        usage.extend(scan_source(&text, &file.display().to_string())?);
    }
    usage.sort();
    usage.dedup();
    Ok(usage)
}

/// Every governed call site in one source file.
///
/// # Errors
///
/// [`Error::Validation`] when a marker is malformed, when it names a kind
/// outside the vocabulary, or when a `Governed::new` site has neither literal
/// arguments nor a marker above it.
pub fn scan_source(text: &str, origin: &str) -> Result<Vec<Usage>, Error> {
    let mut usage = Vec::new();
    let mut marker_lines = Vec::new();

    let mut at = 0usize;
    while let Some(found) = text.get(at..).and_then(|rest| rest.find(MARKER)) {
        let start = at + found;
        let line = line_of(text, start);
        marker_lines.push(line);
        let open = start + MARKER.len() - 1;
        let (inner_start, inner_end) = balanced(text, open, b'(', b')').ok_or_else(|| {
            Error::Validation(format!(
                "{origin}:{line}: the #[governed(...)] marker is not closed"
            ))
        })?;
        let inner = text.get(inner_start..inner_end).unwrap_or_default();
        usage.push(marker_usage(inner, origin, line)?);
        at = inner_end;
    }

    let mut at = 0usize;
    while let Some(found) = text.get(at..).and_then(|rest| rest.find(CONSTRUCTOR)) {
        let start = at + found;
        let line = line_of(text, start);
        let open = start + CONSTRUCTOR.len() - 1;
        let (inner_start, inner_end) = balanced(text, open, b'(', b')').ok_or_else(|| {
            Error::Validation(format!(
                "{origin}:{line}: the Governed::new(...) call is not closed"
            ))
        })?;
        at = inner_end;

        let inner = text.get(inner_start..inner_end).unwrap_or_default();
        let args = split_args(inner);
        match constructor_usage(&args, origin, line) {
            Some(found) => usage.push(found?),
            None => {
                let covered = marker_lines
                    .iter()
                    .any(|m| *m <= line && line.saturating_sub(*m) <= MARKER_LOOKBACK_LINES);
                if !covered {
                    return Err(Error::Validation(format!(
                        "{origin}:{line}: this Governed::new call does not name its service, \
                         kind, and resource as literals, and no #[governed(...)] marker sits \
                         within {MARKER_LOOKBACK_LINES} lines above it; absence is never \
                         permission"
                    )));
                }
            }
        }
    }

    usage.sort();
    usage.dedup();
    Ok(usage)
}

/// Check observed usage against the manifest's grants.
///
/// # Errors
///
/// [`Error::Validation`] listing every call site the manifest does not cover,
/// with the `(service, kind, resource)` each one needs.
pub fn verify_usage(manifest: &Manifest, usage: &[Usage]) -> Result<(), Error> {
    let mut missing = Vec::new();
    for used in usage {
        let covered = ServiceName::parse(used.service.clone())
            .is_ok_and(|service| manifest.covers(&service, used.kind, &used.resource));
        if !covered {
            missing.push(used.to_string());
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "{} governed call site(s) are outside the manifest's ceiling; declare a capability and \
         grant it, or delete the call:\n  {}",
        missing.len(),
        missing.join("\n  ")
    )))
}

/// Verify the calling crate against `manifest` (spec 015 B-3).
///
/// The single-argument form walks the crate that invokes it: `env!` expands
/// while that crate is compiled, so `CARGO_MANIFEST_DIR` is its directory
/// whether the call is in a `build.rs` or in a test. The two-argument form
/// takes an explicit crate root, which is what a test of the verifier itself
/// needs.
///
/// Both forms evaluate to `Result<(), rahi_types::Error>`.
#[macro_export]
macro_rules! verify {
    ($manifest:expr) => {
        $crate::verify::verify_crate(
            $manifest,
            ::std::path::Path::new(::std::env!("CARGO_MANIFEST_DIR")),
        )
    };
    ($manifest:expr, $root:expr) => {
        $crate::verify::verify_crate($manifest, ::std::path::Path::new($root))
    };
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = fs::read_dir(dir)
        .map_err(|e| Error::Io(format!("{} cannot be listed: {e}", dir.display())))?;
    for entry in entries {
        let entry =
            entry.map_err(|e| Error::Io(format!("{} cannot be listed: {e}", dir.display())))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Read `service = "..", kind = "..", resource = ".."` out of a marker.
fn marker_usage(inner: &str, origin: &str, line: usize) -> Result<Usage, Error> {
    let mut service = None;
    let mut kind = None;
    let mut resource = None;
    for field in split_args(inner) {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let value = unquote(value.trim());
        match key.trim() {
            "service" => service = value,
            "kind" => kind = value,
            "resource" => resource = value,
            other => {
                return Err(Error::Validation(format!(
                    "{origin}:{line}: #[governed(...)] has no {other:?} key; it takes service, \
                     kind, and resource"
                )));
            }
        }
    }
    let (Some(service), Some(kind), Some(resource)) = (service, kind, resource) else {
        return Err(Error::Validation(format!(
            "{origin}:{line}: #[governed(...)] names all three of service, kind, and resource \
             as string literals"
        )));
    };
    Ok(Usage {
        service,
        kind: CapabilityKind::parse(&kind)
            .map_err(|e| Error::Validation(format!("{origin}:{line}: {}", e.message())))?,
        resource,
        site: format!("{origin}:{line}"),
    })
}

/// Read the triple out of a `Governed::new` argument list.
///
/// `None` when the call is opaque: the arguments are there but not literals,
/// so the caller looks for a marker instead.
fn constructor_usage(args: &[&str], origin: &str, line: usize) -> Option<Result<Usage, Error>> {
    let service = unquote(args.get(1)?.trim())?;
    let kind = kind_of(args.get(2)?.trim())?;
    let resource = unquote(args.get(3)?.trim())?;
    Some(Ok(Usage {
        service,
        kind,
        resource,
        site: format!("{origin}:{line}"),
    }))
}

/// A kind written as `CapabilityKind::DbWrite` or as `"db.write"`.
fn kind_of(arg: &str) -> Option<CapabilityKind> {
    if let Some(text) = unquote(arg) {
        return CapabilityKind::parse(&text).ok();
    }
    let variant = arg.rsplit("::").next()?.trim();
    CapabilityKind::ALL
        .into_iter()
        .find(|kind| format!("{kind:?}") == variant)
}

/// The contents of a `"..."` literal, or `None` when `arg` is not one.
fn unquote(arg: &str) -> Option<String> {
    let body = arg.strip_prefix('"')?.strip_suffix('"')?;
    if body.contains('"') {
        return None;
    }
    Some(body.replace("\\\\", "\\"))
}

/// The 1-based line `offset` falls on.
fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .unwrap_or_default()
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

/// The span between the delimiters opened at `from`, exclusive of both.
fn balanced(text: &str, from: usize, open: u8, close: u8) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start = None;
    let mut in_str = false;
    let mut escaped = false;
    for index in from..bytes.len() {
        let byte = *bytes.get(index)?;
        if in_str {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_str = false;
            }
            continue;
        }
        if byte == b'"' {
            in_str = true;
        } else if byte == open {
            depth += 1;
            if depth == 1 {
                start = Some(index + 1);
            }
        } else if byte == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return start.map(|s| (s, index));
            }
        }
    }
    None
}

/// Split an argument list on its top-level commas.
fn split_args(inner: &str) -> Vec<&str> {
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    let mut from = 0usize;
    for index in 0..bytes.len() {
        let Some(byte) = bytes.get(index).copied() else {
            break;
        };
        if in_str {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_str = false;
            }
            continue;
        }
        match byte {
            b'"' => in_str = true,
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                out.push(inner.get(from..index).unwrap_or_default().trim());
                from = index + 1;
            }
            _ => {}
        }
    }
    let tail = inner.get(from..).unwrap_or_default().trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_constructor_names_its_triple() {
        let src = r#"
            let notes = Governed::new(&kernel, "notes", CapabilityKind::DbWrite, "notes", store)?;
        "#;
        let usage = scan_source(src, "x.rs").expect("scans");
        assert_eq!(usage.len(), 1);
        let found = usage.first().expect("one usage");
        assert_eq!(found.service, "notes");
        assert_eq!(found.kind, CapabilityKind::DbWrite);
        assert_eq!(found.resource, "notes");
    }

    #[test]
    fn a_marker_names_a_triple_a_literal_cannot() {
        let src = r#"
            // #[governed(service = "notes", kind = "kv.put", resource = "cache")]
            let cache = Governed::new(&kernel, svc, kind, resource, store)?;
        "#;
        let usage = scan_source(src, "x.rs").expect("scans");
        assert_eq!(usage.len(), 1);
        assert_eq!(usage.first().map(|u| u.kind), Some(CapabilityKind::KvPut));
    }

    #[test]
    fn an_opaque_call_with_no_marker_is_a_build_failure() {
        let src = "let x = Governed::new(&kernel, svc, kind, resource, store)?;";
        let err = scan_source(src, "x.rs").expect_err("refused");
        assert!(
            err.message().contains("absence is never permission"),
            "{err}"
        );
    }

    #[test]
    fn a_marker_kind_outside_the_vocabulary_is_refused() {
        let src = r#"// #[governed(service = "a", kind = "db.drop", resource = "b")]"#;
        let err = scan_source(src, "x.rs").expect_err("refused");
        assert!(err.message().contains("db.drop"), "{err}");
    }
}
