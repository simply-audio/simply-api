//! Who is told `notifications/tools/list_changed`, on either kind of MCP
//! lifecycle.
//!
//! A client on a revision with an `initialize` handshake is told on its
//! session, once it has sent `initialized`. A `2026-07-28` client has no
//! handshake and asks for the notification with `subscriptions/listen`,
//! which holds a stream open until the client cancels it. A handler that
//! advertises `tools.listChanged` keeps one [`ListChanged`] and feeds it
//! from `on_initialized` and `listen`.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rmcp::model::SubscriptionFilter;
use rmcp::service::{Peer, RoleServer, SubscriptionSink};

/// How long one listener gets to take a notification.
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(5);

/// The session and the open subscriptions to tell when the tools change.
#[derive(Default)]
pub struct ListChanged {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The connected session, once it has sent `initialized`.
    session: Option<Peer<RoleServer>>,
    /// Open `subscriptions/listen` streams, by a key of our own (request ids
    /// repeat across sessions).
    subscriptions: BTreeMap<u64, SubscriptionSink>,
    next: u64,
}

impl ListChanged {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The session to tell, or none once it has gone.
    pub fn set_session(&self, peer: Option<Peer<RoleServer>>) {
        self.lock().session = peer;
    }

    /// Tell `sink` of every change until [`ListChanged::unsubscribe`] is
    /// called with the key this returns.
    pub fn subscribe(&self, sink: SubscriptionSink) -> u64 {
        let mut inner = self.lock();
        let key = inner.next;
        inner.next += 1;
        inner.subscriptions.insert(key, sink);
        key
    }

    pub fn unsubscribe(&self, key: u64) {
        self.lock().subscriptions.remove(&key);
    }

    /// How many open subscriptions would be told.
    pub fn subscriptions(&self) -> usize {
        self.lock().subscriptions.len()
    }

    /// Tell every listener, on `runtime`, each given a few seconds, without
    /// waiting for any of them.
    pub fn notify(&self, runtime: &tokio::runtime::Handle) {
        let (session, sinks) = {
            let inner = self.lock();
            (inner.session.clone(), inner.subscriptions.values().cloned().collect::<Vec<_>>())
        };
        if let Some(peer) = session {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(NOTIFY_TIMEOUT, peer.notify_tool_list_changed()).await;
            });
        }
        for sink in sinks {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(NOTIFY_TIMEOUT, sink.notify_tool_list_changed()).await;
            });
        }
    }

    /// What a handler that only announces tool-list changes accepts from a
    /// `subscriptions/listen` request.
    pub fn filter() -> SubscriptionFilter {
        SubscriptionFilter::builder().tools_list_changed().build()
    }
}
