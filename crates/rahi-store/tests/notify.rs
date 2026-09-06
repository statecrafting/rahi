//! spec 012 FR-005: a local publish reaches a local listener, and the
//! envelope is a routing key and nothing else.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

mod common;

use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use rahi_store::{Envelope, Notify};
use rahi_types::Revision;

/// The next envelope through the [`Stream`] impl, so the trait is exercised
/// and not merely declared.
async fn next<S>(stream: &mut S) -> Option<Envelope>
where
    S: Stream<Item = Envelope> + Unpin,
{
    let received = std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx));
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .expect("an envelope arrives within five seconds")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listener_receives_an_envelope_published_on_the_same_node() {
    let f = common::open().await;
    let notify = Notify::new(f.store.handle());
    let mut listen = notify.listen();

    let sent = Envelope::new("note", None, "n-1", Revision::new(1));
    notify.notify(sent.clone()).await.unwrap();

    assert_eq!(next(&mut listen).await, Some(sent));
    f.store.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_consumer_filters_by_kind() {
    let f = common::open().await;
    let notify = Notify::new(f.store.handle());
    let mut listen = notify.listen();

    for env in [
        Envelope::new("audit", Some("acme".to_owned()), "a-1", Revision::new(1)),
        Envelope::new("note", Some("acme".to_owned()), "n-1", Revision::new(2)),
    ] {
        notify.notify(env).await.unwrap();
    }

    let mut notes = Vec::new();
    while notes.is_empty() {
        let env = listen.recv().await.expect("the stream stays open");
        if env.kind == "note" {
            notes.push(env);
        }
    }
    assert_eq!(notes[0].name, "n-1");
    assert_eq!(notes[0].tenant.as_deref(), Some("acme"));
    assert_eq!(notes[0].revision, Revision::new(2));

    f.store.shutdown().await.unwrap();
}
