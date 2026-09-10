//! hello-cell: the smallest complete governed cell (spec 034).
//!
//! The binary is the composer over one [`cell::HelloCell`]: `serve` and
//! every operational verb of spec 030 come from the chassis; the app is the
//! manifest, one migration, the notes resource, an operator route, and a
//! page.

#![forbid(unsafe_code)]

mod cell;
mod migrations;
mod notes;

fn main() {
    rahi_cli::run(cell::HelloCell);
}
