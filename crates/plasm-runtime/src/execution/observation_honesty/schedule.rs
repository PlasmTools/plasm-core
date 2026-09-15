//! Barrier + explicit release schedule — no sleeps.
//!
//! Requests park on a oneshot until the driver releases them in a chosen order.
//! The driver asserts that the expected number of requests were simultaneously
//! in flight before any release.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{oneshot, Notify};

/// Shared parking lot for in-flight HTTP requests under a controlled schedule.
#[derive(Debug)]
pub struct BarrierSchedule {
    inner: Mutex<Inner>,
    arrived: Notify,
    /// High-water mark of concurrently parked (not-yet-released) requests.
    max_in_flight: AtomicUsize,
}

#[derive(Debug, Default)]
struct Inner {
    /// Arrived, not yet released: key → release sender.
    pending: HashMap<String, oneshot::Sender<()>>,
    /// Keys that have been released (for diagnostics).
    released: Vec<String>,
    /// Arrival order (registration order).
    arrival_order: Vec<String>,
    completed: Vec<String>,
}

impl BarrierSchedule {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            arrived: Notify::new(),
            max_in_flight: AtomicUsize::new(0),
        })
    }

    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn arrival_order(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("schedule lock")
            .arrival_order
            .clone()
    }

    /// Park until the driver releases `key`. Counts toward in-flight while parked.
    pub async fn park(&self, key: String) {
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.inner.lock().expect("schedule lock");
            assert!(!g.pending.contains_key(&key), "duplicate park key {key}");
            g.pending.insert(key.clone(), tx);
            g.arrival_order.push(key);
            let n = g.pending.len();
            self.max_in_flight.fetch_max(n, Ordering::SeqCst);
        }
        self.arrived.notify_one();
        // If the sender was dropped without send, treat as released-forcibly.
        let _ = rx.await;
    }

    /// Block until at least `n` requests are parked (in flight, awaiting release).
    pub async fn wait_until_parked(&self, n: usize) {
        loop {
            {
                let g = self.inner.lock().expect("schedule lock");
                if g.pending.len() >= n {
                    return;
                }
            }
            self.arrived.notified().await;
        }
    }

    pub fn complete(&self, key: &str) {
        self.inner
            .lock()
            .expect("schedule lock")
            .completed
            .push(key.to_owned());
        self.arrived.notify_one();
    }

    pub fn completion_order(&self) -> Vec<String> {
        self.inner.lock().expect("schedule lock").completed.clone()
    }

    /// Release one parked request by key.
    pub fn release(&self, key: &str) {
        let tx = {
            let mut g = self.inner.lock().expect("schedule lock");
            g.pending.remove(key).unwrap_or_else(|| {
                panic!(
                    "release unknown or already-released key {key}; pending={:?} released={:?}",
                    g.pending.keys().collect::<Vec<_>>(),
                    g.released
                )
            })
        };
        let mut g = self.inner.lock().expect("schedule lock");
        g.released.push(key.to_string());
        drop(g);
        let _ = tx.send(());
    }

    /// Release whatever is parked in arbitrary pending order (serial / teardown).
    #[allow(dead_code)]
    pub fn release_all_arrived(&self) {
        let keys: Vec<String> = {
            let g = self.inner.lock().expect("schedule lock");
            g.pending.keys().cloned().collect()
        };
        for key in keys {
            self.release(&key);
        }
    }
}

/// Drive a concurrent wave: wait for all keys to park, assert in-flight, release by schedule.
pub async fn run_release_wave(schedule: &BarrierSchedule, release_order: &[String]) {
    let n = release_order.len();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        schedule.wait_until_parked(n),
    )
    .await
    .expect("requests did not reach hydration barrier");
    assert!(
        schedule.max_in_flight() >= n,
        "expected ≥{n} in flight before release, got {}",
        schedule.max_in_flight()
    );
    // Every key in the release order must be parked.
    {
        let g = schedule.inner.lock().expect("schedule lock");
        for key in release_order {
            assert!(
                g.pending.contains_key(key),
                "release order key {key} not parked; pending={:?}",
                g.pending.keys().collect::<Vec<_>>()
            );
        }
    }
    for (index, key) in release_order.iter().enumerate() {
        schedule.release(key);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if schedule.completion_order().len() > index {
                    break;
                }
                schedule.arrived.notified().await;
            }
        })
        .await
        .expect("released response did not complete");
        assert_eq!(schedule.completion_order(), release_order[..=index]);
    }
}
