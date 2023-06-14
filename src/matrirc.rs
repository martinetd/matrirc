use anyhow::{Context, Result};
use matrix_sdk::Client;
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

#[derive(Clone, Copy, PartialEq)]
enum Running {
    First,
    Nominal,
    Stop,
}

struct MatrircInner {
    matrix: Client,
    irc: IrcClient,
    /// stop indicator
    running: RwLock<Running>,
    /// room mappings in both directions
    /// implementation in matrix/room_mappings.rs
    mappings: Mappings,
}

impl Matrirc {
    pub fn new(matrix: Client, irc: IrcClient) -> Matrirc {
        Matrirc {
            inner: Arc::new(MatrircInner {
                matrix,
                irc,
                running: RwLock::new(Running::First),
                mappings: Mappings::default(),
            }),
        }
    }

    pub fn irc(&self) -> &IrcClient {
        &self.inner.irc
    }
    pub fn matrix(&self) -> &Client {
        &self.inner.matrix
    }
    pub fn mappings(&self) -> &Mappings {
        &self.inner.mappings
    }
    pub async fn first_sync(&self) -> bool {
        *self.inner.running.read().await == Running::First
    }
    pub async fn running(&self) -> bool {
        let mut running = *self.inner.running.read().await;
        if running == Running::First {
            let mut wr = self.inner.running.write().await;
            running = *wr;
            if running == Running::First {
                *wr = Running::Nominal
            }
        }
        running != Running::Stop
    }
    pub async fn stop<'a, S: Into<String>>(&self, reason: S) -> Result<()> {
        *self.inner.running.write().await = Running::Stop;
        self.inner
            .irc
            .send(ircd::proto::error(reason))
            .await
            .context("stop quit message")
    }
}
