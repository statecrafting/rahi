//! spec 010 FR-003 and B-9: the crate's own sources never read the clock or
//! the process environment, and use only the ordered collections.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: [&str; 4] = ["std::time", "std::env", "HashMap", "HashSet"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The line with any `//` comment removed. Doc comments are comments too.
fn strip_comment(line: &str) -> &str {
    line.split("//").next().unwrap_or_default()
}

#[test]
fn library_sources_contain_no_forbidden_reads_outside_comments() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    files.sort();
    assert!(
        files.len() >= 6,
        "expected the six modules of spec 010, found {files:?}"
    );

    let mut offenders = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = strip_comment(line);
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    offenders.push(format!("{}:{}: {needle}", file.display(), n + 1));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "forbidden reads in library code:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_check_itself_sees_through_comments_only() {
    assert_eq!(strip_comment("let x = 1; // std::time"), "let x = 1; ");
    assert_eq!(strip_comment("/// HashMap is not used"), "");
    assert!(strip_comment("use std::collections::HashMap;").contains("HashMap"));
}
