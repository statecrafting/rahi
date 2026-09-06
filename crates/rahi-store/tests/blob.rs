//! spec 016: binary values, the value ceiling, the paged read, and the
//! extension policy.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use rahi_store::{
    Blob, MAX_VALUE_BYTES, Page, Peer, Statement, Store, StoreConfig, StoreHandle, Value,
};
use rahi_types::Error;
use serde::Deserialize;

const TABLE: &str = "CREATE TABLE vectors (id INTEGER PRIMARY KEY, v BLOB, label TEXT)";
const INSERT: &str = "INSERT INTO vectors (id, v, label) VALUES ($1, $2, $3)";

#[derive(Debug, Deserialize)]
struct VecRow {
    id: i64,
    v: Vec<u8>,
}

#[derive(Debug, Deserialize)]
struct BufRow {
    v: serde_bytes::ByteBuf,
}

#[derive(Debug, Deserialize)]
struct BlobRow {
    v: Blob,
}

#[derive(Debug, Deserialize)]
struct MaybeRow {
    v: Option<Vec<u8>>,
}

#[derive(Debug, Deserialize)]
struct Count {
    n: i64,
}

/// A 6 KiB vector that holds a NUL byte and is not valid UTF-8, so a round
/// trip that survives it survives anything an embedding model produces.
fn vector(seed: u8) -> Vec<u8> {
    (0..6144u32)
        .map(|i| (u8::try_from(i % 256).unwrap()).wrapping_add(seed))
        .collect()
}

async fn count(store: &StoreHandle) -> i64 {
    let rows: Vec<Count> = store
        .query("SELECT COUNT(*) AS n FROM vectors", vec![])
        .await
        .unwrap();
    rows[0].n
}

// --- B-1, FR-001: bytes are ordinary, on both read paths ---------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_six_kib_vector_round_trips_byte_identical_on_both_read_paths() {
    let f = common::open().await;
    let store = f.store.handle();
    store.execute(TABLE, vec![]).await.unwrap();

    let owned = vector(0);
    assert!(owned.contains(&0), "the fixture carries a NUL byte");
    assert!(
        String::from_utf8(owned.clone()).is_err(),
        "the fixture is not valid UTF-8"
    );

    let borrowed = vector(7);
    let boxed = Blob::new(vector(13)).unwrap();

    // Three ways to hand the store bytes: an owned vector, a slice, and a
    // checked `Blob`. None of them takes a base64 hop.
    store
        .execute(
            INSERT,
            vec![Value::from(1i64), Value::from(owned.clone()), Value::Null],
        )
        .await
        .unwrap();
    store
        .execute(
            INSERT,
            vec![
                Value::from(2i64),
                Value::from(borrowed.as_slice()),
                Value::Null,
            ],
        )
        .await
        .unwrap();
    store
        .execute(
            INSERT,
            vec![Value::from(3i64), Value::from(boxed.clone()), Value::Null],
        )
        .await
        .unwrap();

    let read = |sql: &'static str| {
        let store = store.clone();
        async move {
            let rows: Vec<VecRow> = store.query(sql, vec![]).await.unwrap();
            rows
        }
    };
    let local = read("SELECT id, v FROM vectors ORDER BY id").await;
    assert_eq!(local.len(), 3);
    assert_eq!(
        local[0].v, owned,
        "the local replica returns the same bytes"
    );
    assert_eq!(local[1].v, borrowed);
    assert_eq!(local[2].v, boxed.as_slice());

    let consistent: Vec<VecRow> = store
        .query_consistent("SELECT id, v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(consistent.len(), 3);
    assert_eq!(
        consistent[0].v, owned,
        "the leader returns the same bytes as the replica"
    );
    assert_eq!(consistent[1].v, borrowed);
    assert_eq!(consistent[2].v, boxed.as_slice());
    assert_eq!(consistent[0].id, 1);

    // The same column, into the other two shapes spec 016 B-1 names.
    let bufs: Vec<BufRow> = store
        .query("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(bufs[0].v.as_ref(), owned.as_slice());
    let bufs: Vec<BufRow> = store
        .query_consistent("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(bufs[0].v.as_ref(), owned.as_slice());

    let blobs: Vec<BlobRow> = store
        .query("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(blobs[0].v.as_slice(), owned.as_slice());
    let blobs: Vec<BlobRow> = store
        .query_consistent("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(blobs[0].v.as_slice(), owned.as_slice());
    assert_eq!(blobs[0].v.len(), 6144);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_null_blob_deserializes_into_none_on_both_read_paths() {
    let f = common::open().await;
    let store = f.store.handle();
    store.execute(TABLE, vec![]).await.unwrap();
    store
        .execute(
            INSERT,
            vec![Value::from(1i64), Value::Null, Value::from("empty")],
        )
        .await
        .unwrap();
    store
        .execute(
            INSERT,
            vec![
                Value::from(2i64),
                Value::from(vec![1u8, 2, 3]),
                Value::from("three"),
            ],
        )
        .await
        .unwrap();

    let local: Vec<MaybeRow> = store
        .query("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(local[0].v, None, "a NULL blob is None, not an empty vector");
    assert_eq!(local[1].v, Some(vec![1u8, 2, 3]));

    let consistent: Vec<MaybeRow> = store
        .query_consistent("SELECT v FROM vectors ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(consistent[0].v, None);
    assert_eq!(consistent[1].v, Some(vec![1u8, 2, 3]));
}

// --- B-2, FR-002: the ceiling, refused before the statement is submitted -----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parameter_over_the_ceiling_is_refused_and_writes_nothing() {
    let f = common::open().await;
    let store = f.store.handle();
    store.execute(TABLE, vec![]).await.unwrap();

    let over = vec![0xABu8; MAX_VALUE_BYTES + 1];
    let err = store
        .execute(
            INSERT,
            vec![Value::from(1i64), Value::from(over.clone()), Value::Null],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(
        err.to_string().contains("Raft log"),
        "the refusal says what a large value costs: {err}"
    );
    assert_eq!(count(&store).await, 0, "no row was written");

    // The same refusal inside a batch, and the batch submits nothing.
    let err = store
        .txn(vec![
            Statement::with_params(
                INSERT,
                vec![Value::from(1i64), Value::from(vec![1u8, 2]), Value::Null],
            ),
            Statement::with_params(
                INSERT,
                vec![Value::from(2i64), Value::from(over.clone()), Value::Null],
            ),
        ])
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    assert!(err.to_string().contains("statement 2"), "{err}");
    assert_eq!(count(&store).await, 0, "the batch was never submitted");

    // A text parameter is replicated the same way and is held to the same
    // ceiling.
    let err = store
        .execute(
            INSERT,
            vec![
                Value::from(1i64),
                Value::Null,
                Value::from("x".repeat(MAX_VALUE_BYTES + 1)),
            ],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");

    // Exactly at the ceiling is a write, not a refusal.
    let at = vec![0x5Au8; MAX_VALUE_BYTES];
    store
        .execute(
            INSERT,
            vec![Value::from(9i64), Value::from(at), Value::Null],
        )
        .await
        .unwrap();
    assert_eq!(count(&store).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ceiling_is_configurable_downward_only() {
    let f = common::open().await;
    let store = f.store.handle();
    store.execute(TABLE, vec![]).await.unwrap();
    assert_eq!(store.max_value_bytes(), MAX_VALUE_BYTES);

    let small = store.with_max_value_bytes(4096).unwrap();
    assert_eq!(small.max_value_bytes(), 4096);
    let err = small
        .execute(
            INSERT,
            vec![Value::from(1i64), Value::from(vec![0u8; 4097]), Value::Null],
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
    // The handle it was lowered from keeps its own ceiling.
    store
        .execute(
            INSERT,
            vec![Value::from(1i64), Value::from(vec![0u8; 4097]), Value::Null],
        )
        .await
        .unwrap();

    assert!(
        matches!(
            small.with_max_value_bytes(MAX_VALUE_BYTES),
            Err(Error::Validation(_))
        ),
        "a lowered ceiling never buys itself back"
    );
    assert!(matches!(
        store.with_max_value_bytes(MAX_VALUE_BYTES + 1),
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        store.with_max_value_bytes(0),
        Err(Error::Validation(_))
    ));

    // `Blob` applies the constant at the boundary where the bytes are made.
    assert!(matches!(
        Blob::new(vec![0u8; MAX_VALUE_BYTES + 1]),
        Err(Error::Validation(_))
    ));
    assert_eq!(Blob::new(vec![0u8; 8]).unwrap().len(), 8);
    assert!(Blob::new(Vec::new()).unwrap().is_empty());
}

// --- B-3, FR-003: a sweep is a loop over pages ------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sweep_of_five_thousand_rows_returns_every_row_exactly_once() {
    let f = common::open().await;
    let store = f.store.handle();
    store.execute(TABLE, vec![]).await.unwrap();

    let rows = 5000i64;
    for chunk in (1..=rows).collect::<Vec<_>>().chunks(500) {
        let batch: Vec<Statement> = chunk
            .iter()
            .map(|id| {
                Statement::with_params(
                    INSERT,
                    vec![
                        Value::from(*id),
                        Value::from(vec![u8::try_from(id % 251).unwrap(); 32]),
                        Value::from("row"),
                    ],
                )
            })
            .collect();
        store.txn(batch).await.unwrap();
    }
    assert_eq!(count(&store).await, rows);

    let mut page = Page::default();
    assert_eq!(page.size, 1024, "the default page is 1024 rows");
    let mut seen: Vec<i64> = Vec::new();
    let mut pages = 0;
    loop {
        let batch: Vec<VecRow> = store
            .query_paged("SELECT id, v FROM vectors ORDER BY id", vec![], page)
            .await
            .unwrap();
        assert!(
            batch.len() <= page.size as usize,
            "a page never holds more than its size"
        );
        if batch.is_empty() {
            break;
        }
        pages += 1;
        seen.extend(batch.iter().map(|r| r.id));
        page = page.next();
    }
    assert_eq!(pages, 5, "5000 rows in pages of 1024 is five pages");
    assert_eq!(seen.len() as i64, rows, "every row came back exactly once");
    assert_eq!(
        seen.iter().copied().collect::<BTreeSet<_>>().len() as i64,
        rows,
        "no row came back twice"
    );
    assert!(
        seen.windows(2).all(|w| w[0] < w[1]),
        "the sweep has a stable order"
    );

    // The caller's own parameters keep their numbers: the appended clause
    // takes the two after them.
    let filtered: Vec<VecRow> = store
        .query_paged(
            "SELECT id, v FROM vectors WHERE id > $1 AND label = $2 ORDER BY id",
            vec![Value::from(4990i64), Value::from("row")],
            Page::new(4),
        )
        .await
        .unwrap();
    assert_eq!(filtered.len(), 4);
    assert_eq!(filtered[0].id, 4991);

    let err = store
        .query_paged::<VecRow>("SELECT id, v FROM vectors", vec![], Page::new(0))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Validation(_)), "{err}");
}

// --- B-4, FR-004: no loadable extensions ------------------------------------

/// The three call shapes that could hand this engine an extension. None of
/// them may appear in the crate's own sources; the store assembles the SQL
/// function's name from halves so this grep needs no exemption list.
const FORBIDDEN: [&str; 3] = [
    "load_extension",
    "enable_load_extension",
    "sqlite3_auto_extension",
];

fn offences(text: &str) -> Vec<&'static str> {
    FORBIDDEN
        .iter()
        .filter(|needle| text.contains(*needle))
        .copied()
        .collect()
}

fn rust_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_crates_own_sources_name_no_extension_call() {
    let mut files = Vec::new();
    rust_sources(Path::new("src"), &mut files);
    assert!(files.len() > 5, "the scan found the crate: {files:?}");
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        assert!(
            offences(&text).is_empty(),
            "{} names {:?}; no node may be handed an extension the others lack (spec 016 B-4)",
            file.display(),
            offences(&text)
        );
    }

    // The grep is only worth having if it fails when it should: each of the
    // three shapes is caught, and ordinary store code is not.
    assert_eq!(
        offences("    conn.enable_load_extension(true)?;"),
        vec!["load_extension", "enable_load_extension"]
    );
    assert_eq!(
        offences("SELECT load_extension('vec0')"),
        vec!["load_extension"]
    );
    assert_eq!(
        offences("unsafe { sqlite3_auto_extension(Some(init)) };"),
        vec!["sqlite3_auto_extension"]
    );
    assert!(offences("store.execute(INSERT, vec![]).await?;").is_empty());
}

#[derive(Debug, Deserialize)]
struct Probe {
    #[allow(dead_code)]
    probe: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_engine_refuses_to_load_an_extension() {
    // `Store::open` already asserted this against its own connection; a store
    // that reached this line booted without loading one. The direct ask is
    // here so the assertion is visible rather than implied. hiqlite drops
    // row-level errors on a local read, so a refused load arrives as no rows;
    // a load that had succeeded would return one row holding NULL.
    let f = common::open().await;
    let store = f.store.handle();
    let answer: Vec<Probe> = store
        .query(
            "SELECT load_extension($1) AS probe",
            vec![Value::from("rahi-no-such-extension")],
        )
        .await
        .unwrap();
    assert!(answer.is_empty(), "the engine loaded nothing");

    let report = store.engine_report();
    assert_eq!(report.extensions, "none");
    assert_eq!(report.max_value_bytes, MAX_VALUE_BYTES);
    assert_eq!(
        report.to_string(),
        format!("extensions: none, max_value_bytes: {MAX_VALUE_BYTES}")
    );
}

// --- FR-005: three nodes, one engine, identical bytes -----------------------

struct Cluster {
    stores: Vec<Store>,
    _dirs: Vec<tempfile::TempDir>,
}

fn cluster_config(
    node_id: u64,
    peers: &[Peer],
    addrs: (SocketAddr, SocketAddr),
    dir: &Path,
) -> StoreConfig {
    StoreConfig {
        node_id,
        nodes: peers.to_vec(),
        data_dir: dir.join("hiqlite"),
        raft_addr: addrs.0,
        api_addr: addrs.1,
        secrets: common::secrets(),
        backup_keep_days: 1,
        s3: None,
    }
}

async fn open_cluster() -> Cluster {
    let dirs: Vec<tempfile::TempDir> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let addrs: Vec<(SocketAddr, SocketAddr)> = (0..3)
        .map(|_| (common::free_addr(), common::free_addr()))
        .collect();
    let peers: Vec<Peer> = addrs
        .iter()
        .enumerate()
        .map(|(i, (raft, api))| Peer {
            id: u64::try_from(i + 1).unwrap(),
            raft_addr: raft.to_string(),
            api_addr: api.to_string(),
        })
        .collect();

    let mut opening = Vec::new();
    for (i, dir) in dirs.iter().enumerate() {
        let cfg = cluster_config(u64::try_from(i + 1).unwrap(), &peers, addrs[i], dir.path());
        opening.push(tokio::spawn(async move { Store::open(&cfg).await }));
    }
    let mut stores = Vec::new();
    for task in opening {
        stores.push(task.await.unwrap().expect("the node joins the cluster"));
    }
    Cluster {
        stores,
        _dirs: dirs,
    }
}

async fn wait_for_local_rows(store: &StoreHandle, expected: i64) {
    for _ in 0..200 {
        if count(store).await == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the follower never caught up to {expected} rows");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_nodes_return_identical_bytes_for_the_same_sweep() {
    let cluster = open_cluster().await;
    let handles: Vec<StoreHandle> = cluster.stores.iter().map(Store::handle).collect();
    handles[0].execute(TABLE, vec![]).await.unwrap();

    let rows = 1000i64;
    let payload = |id: i64| -> Vec<u8> {
        (0..64u32)
            .map(|i| u8::try_from((u64::try_from(id).unwrap() * 31 + u64::from(i)) % 256).unwrap())
            .collect()
    };
    for chunk in (1..=rows).collect::<Vec<_>>().chunks(250) {
        let batch: Vec<Statement> = chunk
            .iter()
            .map(|id| {
                Statement::with_params(
                    INSERT,
                    vec![
                        Value::from(*id),
                        Value::from(payload(*id)),
                        Value::from("n3"),
                    ],
                )
            })
            .collect();
        handles[0].txn(batch).await.unwrap();
    }

    // Every node sweeps the table for itself, in pages, and must return the
    // same bytes in the same order. A node whose engine differed would show
    // up here as a different byte, a missing row, or a failed apply.
    let mut sweeps: Vec<Vec<(i64, Vec<u8>)>> = Vec::new();
    for handle in &handles {
        wait_for_local_rows(handle, rows).await;
        let mut swept: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut offset = 0i64;
        loop {
            let page: Vec<VecRow> = handle
                .query_consistent(
                    "SELECT id, v FROM vectors ORDER BY id LIMIT $1 OFFSET $2",
                    vec![Value::from(250i64), Value::from(offset)],
                )
                .await
                .unwrap();
            if page.is_empty() {
                break;
            }
            offset += i64::try_from(page.len()).unwrap();
            swept.extend(page.into_iter().map(|r| (r.id, r.v)));
        }
        assert_eq!(swept.len() as i64, rows);
        sweeps.push(swept);
    }
    for (id, bytes) in &sweeps[0] {
        assert_eq!(bytes, &payload(*id), "the leader stored what it was given");
    }
    assert_eq!(sweeps[1], sweeps[0], "node 2 agrees with node 1");
    assert_eq!(sweeps[2], sweeps[0], "node 3 agrees with node 1");

    // The same again from each node's own replica, which is the copy a
    // divergent engine would have written differently.
    let mut local: Vec<Vec<(i64, Vec<u8>)>> = Vec::new();
    for handle in &handles {
        let mut swept: Vec<(i64, Vec<u8>)> = Vec::new();
        let mut page = Page::new(250);
        loop {
            let batch: Vec<VecRow> = handle
                .query_paged("SELECT id, v FROM vectors ORDER BY id", vec![], page)
                .await
                .unwrap();
            if batch.is_empty() {
                break;
            }
            swept.extend(batch.into_iter().map(|r| (r.id, r.v)));
            page = page.next();
        }
        local.push(swept);
    }
    assert_eq!(local[0], sweeps[0], "node 1's replica matches the leader");
    assert_eq!(local[1], sweeps[0], "node 2's replica matches the leader");
    assert_eq!(local[2], sweeps[0], "node 3's replica matches the leader");

    // Teardown, not an assertion. Three Rafts standing down together in one
    // process can outlast hiqlite's own shutdown timeout, and what this test
    // is about is the bytes; how a cluster stops is spec 032's business.
    for store in &cluster.stores {
        let _ = store.shutdown().await;
    }
}
