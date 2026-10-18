//! F#18 — in-process real-time broadcast bus.
//!
//! `tokio::sync::broadcast`: any number of SSE clients subscribe
//! independently; every `emit()` fans out to all of them. A subscriber that
//! is slow (or has just connected) is NOT blocked — `broadcast` drops the
//! oldest messages for a lagging receiver and reports `Lagged(n)`, so
//! `emit` never awaits a slow reader. (The UI compensates for lag by
//! re-fetching the DB's `list_notifications` on any `Lagged`, which is
//! why the DB row is the source of truth — see `notifier.rs`.)
//!
//! `Sender`/`Receiver` are cheap `Arc` clones, so a single `LiveBus`
//! instance (inside `AppState`) is shared by every `Notifier` call and
//! every SSE stream without a per-connection channel.

use engine::notify::Notification;
use tokio::sync::broadcast;

const CAPACITY: usize = 256;

/// One alert event on the bus. The DB row id rides along so the Vue UI
/// can dedupe (an event that arrives on the *live* stream AND is already
/// in the list we just fetched is the same row).
#[derive(Clone)]
pub struct LiveEvent {
    pub notification: Notification,
}

pub struct LiveBus {
    tx: broadcast::Sender<LiveEvent>,
}

impl LiveBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        LiveBus { tx }
    }

    /// Broadcast one event. Returns the number of subscribers who actually
    /// received it (0 = nobody is listening — the DB row is still the
    /// source of truth, so no data is lost).
    pub fn send(&self, notification: Notification) -> usize {
        // `send` only fails when there are zero receivers — that's fine
        // (no live clients). We don't surface it: the DB write is the
        // authoritative delivery, and the Vue store re-fetches on
        // `EventSource` (re)connect.
        self.tx.send(LiveEvent { notification }).unwrap_or(0)
    }

    /// Subscribe for the SSE stream. The returned receiver is a normal
    /// `tokio::sync::broadcast::Receiver` — the SSE handler adapts it to
    /// an `axum::response::sse::Event` stream in `api::sse`.
    pub fn subscribe(&self) -> broadcast::Receiver<LiveEvent> {
        self.tx.subscribe()
    }
}

impl std::fmt::Debug for LiveBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveBus").finish_non_exhaustive()
    }
}

impl Default for LiveBus {
    fn default() -> Self {
        Self::new()
    }
}
