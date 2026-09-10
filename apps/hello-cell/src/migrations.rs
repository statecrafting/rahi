//! The one migration (spec 034 B-2): the notes table.
//!
//! `revision` is the watermark column (`rahi_store::Watermark`), stamped by
//! the same transaction as the write; `fence` is the sequence a later spec's
//! fenced writer would compare, kept at zero here.

use std::sync::LazyLock;

use rahi_store::Migration;

/// The migrations, in version order: the chassis's coordination tables
/// first (the fence and the outbox of spec 012, which an app composes into
/// its own list), then the notes table.
pub static MIGRATIONS: LazyLock<Vec<Migration>> = LazyLock::new(|| {
    vec![
        rahi_store::coordination_migration(1),
        Migration::new(
            2,
            "notes",
            "CREATE TABLE IF NOT EXISTS notes (\
            id TEXT PRIMARY KEY, \
            sub TEXT NOT NULL, \
            body TEXT NOT NULL, \
            revision INTEGER NOT NULL DEFAULT 0, \
            fence INTEGER NOT NULL DEFAULT 0, \
            created_at INTEGER NOT NULL\
        )",
        ),
    ]
});
