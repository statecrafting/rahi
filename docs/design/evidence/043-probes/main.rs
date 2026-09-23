#[path = "../../common.rs"] mod common;
use common::*;
#[tokio::main]
async fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let mode = std::env::args().nth(2).unwrap_or_default();
    let mut held = Vec::new();
    if mode == "selfhold-owner" || mode == "selfhold-both" || mode == "hold-release" {
        held.push(hold(&format!("{dir}/hiqlite-owner.lock")));
    }
    if mode == "selfhold-wal" || mode == "selfhold-both" || mode == "hold-release" {
        std::fs::create_dir_all(format!("{dir}/logs")).unwrap();
        held.push(hold(&format!("{dir}/logs/lock.hql")));
    }
    if mode == "hold-release" {
        drop(std::mem::take(&mut held));
        println!("RELEASED own fds before start");
    }
    let t0 = std::time::Instant::now();
    let client = match hiqlite::start_node_with_cache::<Cache>(cfg(&dir)).await {
        Ok(c) => c,
        Err(e) => { println!("START_ERR after {:?}: {e}", t0.elapsed()); std::process::exit(3); }
    };
    client.wait_until_healthy_db().await;
    client.wait_until_healthy_cache().await;
    println!("started in {:?}", t0.elapsed());
    if mode == "write" {
        client.execute("CREATE TABLE IF NOT EXISTS t (k TEXT PRIMARY KEY, v TEXT)", vec![]).await.unwrap();
        client.execute("INSERT OR REPLACE INTO t VALUES ('app','row')", vec![]).await.unwrap();
        client.put(Cache::Kv, "revoked-jti", &"yes".to_string(), None).await.unwrap();
        client.counter_add(Cache::Kv, "rate", 5).await.unwrap();
    }
    if mode == "floor" {
        client.execute("CREATE TABLE IF NOT EXISTS floor (id TEXT PRIMARY KEY, before INTEGER)", vec![]).await.unwrap();
        client.execute("INSERT OR REPLACE INTO floor VALUES ('x', 1)", vec![]).await.unwrap();
    }
    let row: Vec<hiqlite::Row> = client.query_raw("SELECT v FROM t WHERE k='app'", vec![]).await.unwrap_or_default();
    let v: Option<String> = client.get(Cache::Kv, "revoked-jti").await.unwrap_or(None);
    let c = client.counter_get(Cache::Kv, "rate").await.ok().flatten();
    println!("sql_rows={} cache_jti={:?} counter={:?}", row.len(), v, c);
    if mode == "hang" { println!("HANGING"); loop { tokio::time::sleep(std::time::Duration::from_secs(3600)).await; } }
    let s = std::time::Instant::now();
    let r = client.shutdown().await;
    println!("shutdown={:?} in {:?}", r.map_err(|e| e.to_string()), s.elapsed());
}
