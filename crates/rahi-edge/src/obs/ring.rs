//! The bounded ring of recent traces (spec 023 B-3).
//!
//! A cell with no collector still has to be able to answer "what happened in
//! the last minute", to an operator over a shell or to the harness (spec
//! 033). The ring is that answer: it is filled whether or not an exporter is
//! configured, it is bounded so that a cell under load cannot grow memory by
//! being observed, and it is a module rather than an API. An app that wants
//! to expose it puts it behind the operator role (spec 024).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use tokio::sync::broadcast;

/// How many traces the ring holds unless the operator says otherwise.
pub const DEFAULT_CAPACITY: usize = 1_000;
/// The variable that tunes it.
pub const ENV_RING_CAPACITY: &str = "RAHI_TRACE_RING_CAPACITY";
/// How many traces a subscriber may fall behind before it misses some.
pub const SUBSCRIBER_LAG: usize = 64;

/// One span, as it was when it closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanRecord {
    /// The span's name, such as `http.request`.
    pub name: String,
    /// How long it was open, in milliseconds.
    pub duration_ms: u64,
    /// Every field recorded on it, rendered.
    pub fields: BTreeMap<String, String>,
}

impl SpanRecord {
    /// The value of `field`, if the span carries one.
    #[must_use]
    pub fn field(&self, field: &str) -> Option<&str> {
        self.fields.get(field).map(String::as_str)
    }
}

/// One request, root span first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// The trace id, which the exported spans carry as an attribute.
    pub id: String,
    /// The request span.
    pub root: SpanRecord,
    /// Every span that closed inside it, in the order they closed.
    pub children: Vec<SpanRecord>,
}

/// A bounded ring of completed traces.
///
/// The oldest is evicted when a new one arrives at capacity, so the memory a
/// cell spends on observing itself is fixed at boot.
#[derive(Debug)]
pub struct Ring {
    capacity: usize,
    traces: Mutex<VecDeque<Trace>>,
    sender: broadcast::Sender<Trace>,
}

impl Ring {
    /// A ring that holds `capacity` traces. A capacity of zero holds one:
    /// a ring that cannot hold anything is a ring that is silently off.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(SUBSCRIBER_LAG);
        Self {
            capacity,
            traces: Mutex::new(VecDeque::with_capacity(capacity)),
            sender,
        }
    }

    /// How many traces the ring holds at most.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Add `trace`, evicting the oldest if the ring is full.
    pub fn push(&self, trace: Trace) {
        {
            let mut traces = self.traces.lock().unwrap_or_else(PoisonError::into_inner);
            if traces.len() == self.capacity {
                traces.pop_front();
            }
            traces.push_back(trace.clone());
        }
        // A send with no receivers is not a failure: nobody is watching.
        let _ = self.sender.send(trace);
    }

    /// Every trace held, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<Trace> {
        self.traces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// The trace with this id, if it has not been evicted.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Trace> {
        self.traces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|trace| trace.id == id)
            .cloned()
    }

    /// How many traces are held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.traces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether the ring is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Watch traces as they complete.
    ///
    /// A receiver that falls more than [`SUBSCRIBER_LAG`] behind misses
    /// traces rather than holding them: the ring's bound is the point.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Trace> {
        self.sender.subscribe()
    }
}

impl Default for Ring {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(id: &str) -> Trace {
        Trace {
            id: id.to_owned(),
            root: SpanRecord {
                name: "http.request".to_owned(),
                duration_ms: 1,
                fields: BTreeMap::new(),
            },
            children: Vec::new(),
        }
    }

    #[test]
    fn the_oldest_goes_when_the_ring_is_full() {
        let ring = Ring::with_capacity(3);
        for id in ["a", "b", "c"] {
            ring.push(trace(id));
        }
        assert_eq!(ring.len(), 3);
        assert!(ring.get("a").is_some());

        ring.push(trace("d"));
        assert_eq!(ring.len(), 3, "the ring is still bounded");
        assert!(ring.get("a").is_none(), "the oldest was evicted");
        assert!(ring.get("d").is_some(), "the newest is held");
        assert_eq!(
            ring.list().into_iter().map(|t| t.id).collect::<Vec<_>>(),
            vec!["b", "c", "d"]
        );
    }

    #[test]
    fn a_ring_that_holds_nothing_holds_one() {
        let ring = Ring::with_capacity(0);
        assert_eq!(ring.capacity(), 1);
        ring.push(trace("a"));
        assert_eq!(ring.len(), 1);
    }
}
