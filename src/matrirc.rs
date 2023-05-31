use anyhow::{Context, Result};
use matrix_sdk::Client;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{ircd, ircd::IrcClient};

/// client state struct
#[derive(Clone, Debug)]
pub struct Matrirc {
    /// wrap in Arc for clone
    inner: Arc<MatrircInner>,
}

#[derive(Debug)]
struct MatrircInner {
    matrix: Client,
    irc: IrcClient,
    running: RwLock<bool>,
}

impl Matrirc {
    pub fn new(matrix: Client, irc: IrcClient) -> Matrirc {
        Matrirc {
            inner: Arc::new(MatrircInner {
                matrix,
                irc,
                running: RwLock::new(true),
            }),
        }
    }

    pub fn irc(&self) -> &IrcClient {
        &self.inner.irc
    }
    pub fn matrix(&self) -> &Client {
        &self.inner.matrix
    }
    pub async fn running(&self) -> bool {
        *self.inner.running.read().await
    }
    pub async fn stop<'a, S: Into<String>>(&self, reason: S) -> Result<()> {
        *self.inner.running.write().await = false;
        self.inner
            .irc
            .send(ircd::proto::error(reason))
            .await
            .context("stop quit message")
    }
}
