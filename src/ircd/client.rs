use anyhow::Result;
use irc::client::prelude::Message;
use tokio::sync::{mpsc, Mutex};

use crate::ircd::proto;

#[derive(Debug)]
pub struct IrcClient {
    /// Avoid waiting on network: queue messages for another task
    /// to actually do the sending.
    /// read in one place and kept private
    pub sink: Mutex<mpsc::Sender<Message>>,
}

impl IrcClient {
    pub fn new(sink: mpsc::Sender<Message>) -> IrcClient {
        IrcClient {
            sink: Mutex::new(sink),
        }
    }

    pub async fn send(&self, msg: Message) -> Result<()> {
        self.sink.lock().await.send(msg).await?;
        Ok(())
    }

    pub async fn send_privmsg<'a, S, T, U>(&self, from: S, target: T, msg: U) -> Result<()>
    where
        S: Into<String>,
        T: Into<String>,
        U: Into<String>,
    {
        self.send(proto::privmsg(from, target, msg)).await
    }
}
