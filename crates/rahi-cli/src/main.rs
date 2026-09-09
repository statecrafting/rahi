//! The `rahi` binary: the chassis with no app (spec 030 AC-2).

#![forbid(unsafe_code)]

fn main() {
    rahi_cli::run(rahi_cli::EmptyCell)
}
