//! Spec 037 B-2: one budget includes queueing, and accepted work keeps its gate.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rahi_store::backup::BackupDeadline;
use rahi_types::Error;
use tokio::sync::{Mutex, oneshot};
use tokio::time::Instant;

fn assert_deadline(error: Error, budget: Duration) {
    assert!(matches!(error, Error::Upstream(_)), "{error}");
    assert!(error.to_string().contains("deadline"), "{error}");
    assert!(
        error
            .to_string()
            .contains(&format!("{} ms", budget.as_millis())),
        "{error}"
    );
}

#[tokio::test(start_paused = true)]
async fn queueing_consumes_the_budget_without_starting_work() {
    static GATE: Mutex<()> = Mutex::const_new(());
    let guard = GATE.lock().await;
    let budget = Duration::from_millis(100);
    let started = Instant::now();
    let ran = Arc::new(AtomicBool::new(false));
    let worker_ran = Arc::clone(&ran);

    let error = BackupDeadline::new(budget)
        .serialised(&GATE, async move {
            worker_ran.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();

    assert_deadline(error, budget);
    assert_eq!(started.elapsed(), budget);
    assert!(!ran.load(Ordering::SeqCst));
    drop(guard);
    BackupDeadline::new(budget)
        .serialised(&GATE, async { Ok(()) })
        .await
        .unwrap();
    assert!(!ran.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn accepted_work_keeps_the_gate_after_the_shared_budget_expires() {
    static GATE: Mutex<()> = Mutex::const_new(());
    let guard = GATE.lock().await;
    let budget = Duration::from_millis(100);
    let deadline = BackupDeadline::new(budget);
    let started = Instant::now();
    let (entered_tx, entered_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let next_phase = Arc::new(AtomicBool::new(false));
    let worker_next_phase = Arc::clone(&next_phase);
    let caller = tokio::spawn(async move {
        deadline
            .serialised(&GATE, async move {
                entered_tx.send(()).unwrap();
                deadline
                    .observe(async {
                        finish_rx.await.unwrap();
                        Ok(())
                    })
                    .await?;
                worker_next_phase.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(60)).await;
    drop(guard);
    entered_rx.await.unwrap();

    assert_deadline(caller.await.unwrap().unwrap_err(), budget);
    assert_eq!(started.elapsed(), budget);
    assert!(
        GATE.try_lock().is_err(),
        "accepted work still owns the gate"
    );
    assert!(!next_phase.load(Ordering::SeqCst));

    finish_tx
        .send(())
        .expect("the accepted receiver was retained");
    let settled = GATE.lock().await;
    assert!(!next_phase.load(Ordering::SeqCst));
    drop(settled);
    BackupDeadline::new(budget)
        .serialised(&GATE, async { Ok(()) })
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn observing_a_request_lets_it_settle_but_rejects_its_late_success() {
    let budget = Duration::from_millis(100);
    let settled = AtomicBool::new(false);
    let started = Instant::now();

    let error = BackupDeadline::new(budget)
        .observe(async {
            tokio::time::sleep(budget * 2).await;
            settled.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();

    assert_deadline(error, budget);
    assert!(settled.load(Ordering::SeqCst));
    assert_eq!(started.elapsed(), budget * 2);
}

#[tokio::test]
async fn waiting_rejects_a_non_yielding_futures_late_success() {
    let budget = Duration::from_millis(5);
    let settled = AtomicBool::new(false);

    let error = BackupDeadline::new(budget)
        .wait(async {
            // A timeout cannot interrupt this poll. The result still must
            // be checked against the deadline after the poll completes.
            std::thread::sleep(Duration::from_millis(20));
            settled.store(true, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap_err();

    assert_deadline(error, budget);
    assert!(settled.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn cancelling_the_caller_keeps_the_gate_until_accepted_work_settles() {
    static GATE: Mutex<()> = Mutex::const_new(());
    let (entered_tx, entered_rx) = oneshot::channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let settled = Arc::new(AtomicBool::new(false));
    let worker_settled = Arc::clone(&settled);
    let caller = tokio::spawn(async move {
        BackupDeadline::new(Duration::from_secs(1))
            .serialised(&GATE, async move {
                entered_tx.send(()).unwrap();
                finish_rx.await.unwrap();
                worker_settled.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
    });
    entered_rx.await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(GATE.try_lock().is_err());

    let queued_budget = Duration::from_millis(100);
    let queued_error = BackupDeadline::new(queued_budget)
        .serialised::<(), _>(&GATE, async {
            panic!("cancelled callers must retain the gate")
        })
        .await
        .unwrap_err();
    assert_deadline(queued_error, queued_budget);
    assert!(!settled.load(Ordering::SeqCst));

    finish_tx
        .send(())
        .expect("cancellation retained the receiver");
    let guard = GATE.lock().await;
    assert!(settled.load(Ordering::SeqCst));
    drop(guard);
    BackupDeadline::new(queued_budget)
        .serialised(&GATE, async { Ok(()) })
        .await
        .unwrap();
}
