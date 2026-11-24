//! In-process play hub: push PROTOCOL JSON to a user's open sockets (CV-172).
//!
//! Single-process only; multi-instance fan-out stays a later task.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

/// Outbound channel capacity bound (drop oldest is not used; unbounded for M1).
pub type OutboundTx = mpsc::UnboundedSender<Value>;

/// Unique connection id within this process.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ConnId(pub u64);

/// Registry of authenticated user sockets for GameStart / MoveApplied fan-out.
#[derive(Default)]
pub struct PlayHub {
    next_id: AtomicU64,
    inner: Mutex<HashMap<String, HashMap<ConnId, OutboundTx>>>,
}

impl PlayHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an outbound channel for `user_id`. Returns a conn id for unregister.
    pub async fn register(self: &Arc<Self>, user_id: &str, tx: OutboundTx) -> ConnId {
        let id = ConnId(self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let mut guard = self.inner.lock().await;
        guard.entry(user_id.to_owned()).or_default().insert(id, tx);
        id
    }

    /// Drop a connection; removes the user map entry when empty.
    pub async fn unregister(self: &Arc<Self>, user_id: &str, id: ConnId) {
        let mut guard = self.inner.lock().await;
        if let Some(conns) = guard.get_mut(user_id) {
            conns.remove(&id);
            if conns.is_empty() {
                guard.remove(user_id);
            }
        }
    }

    /// True when at least one socket is registered for `user_id`.
    pub async fn is_connected(self: &Arc<Self>, user_id: &str) -> bool {
        let guard = self.inner.lock().await;
        guard.get(user_id).is_some_and(|c| !c.is_empty())
    }

    /// Send `msg` to every open socket for `user_id` (best-effort; closed senders dropped).
    pub async fn send_to_user(self: &Arc<Self>, user_id: &str, msg: Value) {
        let mut dead: Vec<ConnId> = Vec::new();
        {
            let guard = self.inner.lock().await;
            let Some(conns) = guard.get(user_id) else {
                return;
            };
            for (id, tx) in conns {
                if tx.send(msg.clone()).is_err() {
                    dead.push(*id);
                }
            }
        }
        if dead.is_empty() {
            return;
        }
        let mut guard = self.inner.lock().await;
        if let Some(conns) = guard.get_mut(user_id) {
            for id in dead {
                conns.remove(&id);
            }
            if conns.is_empty() {
                guard.remove(user_id);
            }
        }
    }

    /// Fan-out to both seats when they differ.
    pub async fn send_to_seats(self: &Arc<Self>, white_id: &str, black_id: &str, msg: Value) {
        self.send_to_user(white_id, msg.clone()).await;
        if white_id != black_id {
            self.send_to_user(black_id, msg).await;
        }
    }
}
