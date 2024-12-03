use anyhow::{Context, Result};
use lru::LruCache;
use matrix_sdk::{
    ruma::{EventId, OwnedEventId},
    Client,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::matrix::room_mappings::Mappings;
use crate::{ircd, ircd::IrcClient};

/// client state struct
#[derive(Clone)]
pub struct Matrirc {
    /// wrap in Arc for clone
    inner: Arc<MatrircInner>,
}

struct MatrircInner {
    matrix: Client,
    /// stop indicator
    running: RwLock<Running>,
    /// room mappings in both directions
    /// implementation in matrix/room_mappings.rs
    mappings: Mappings,
    /// recent messages (for reactions, redactions)
    recent_messages: RwLock<LruCache<OwnedEventId, String>>,
}

#[derive(Clone, Copy)]
pub enum Running {
    First,
    Continue,
    Break,
}

impl Matrirc {
    pub fn new(matrix: Client, irc: IrcClient) -> Matrirc {
        Matrirc {
            inner: Arc::new(MatrircInner {
                matrix,
                running: RwLock::new(Running::First),
                mappings: Mappings::new(irc),
                recent_messages: RwLock::new(LruCache::new(
                    std::num::NonZeroUsize::new(1000).unwrap(),
                )),
            }),
        }
    }

    pub fn irc(&self) -> &IrcClient {
        &self.mappings().irc
    }
    pub fn matrix(&self) -> &Client {
        &self.inner.matrix
    }
    pub fn mappings(&self) -> &Mappings {
        &self.inner.mappings
    }
    pub async fn running(&self) -> Running {
        // need let to drop read lock
        let v = *self.inner.running.read().await;
        match v {
            Running::First => {
                let mut lock = self.inner.running.write().await;
                match *lock {
                    Running::First => {
                        *lock = Running::Continue;
                        Running::First
                    }
                    run => run,
                }
            }
            run => run,
        }
    }
    pub async fn stop<S: Into<String>>(&self, reason: S) -> Result<()> {
        *self.inner.running.write().await = Running::Break;
        self.irc()
            .send(ircd::proto::error(reason))
            .await
            .context("stop quit message")
    }
    pub async fn message_get(&self, id: &EventId) -> Option<String> {
        self.inner.recent_messages.read().await.peek(id).cloned()
    }
    pub async fn message_put(&self, id: OwnedEventId, message: String) {
        let _ = self.inner.recent_messages.write().await.put(id, message);
    }
}
