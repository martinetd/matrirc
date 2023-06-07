use anyhow::Result;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, TryStreamExt};
use irc::client::prelude::{Command, Message, Prefix};
use irc::proto::IrcCodec;
use log::{info, trace};
use std::cmp::min;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::matrirc::Matrirc;

/// it's a bit of a pain to redo the work twice for notice/privmsg,
/// so these types wrap it around a bit
#[derive(Debug, Clone)]
pub enum IrcMessageType {
    PRIVMSG,
    NOTICE,
}
#[derive(Debug, Clone)]
pub struct IrcMessage {
    pub message_type: IrcMessageType,
    /// source to use for privmsg/similar
    /// (member name for chan, query name for query)
    pub from: String,
    /// target to use for privmsg/similar
    /// (channel name for chan, None for query: in this case use own nick)
    pub target: String,
    /// message content
    pub message: String,
}

impl From<IrcMessage> for Message {
    fn from(message: IrcMessage) -> Self {
        match message.message_type {
            IrcMessageType::PRIVMSG => privmsg(message.from, message.target, message.message),
            IrcMessageType::NOTICE => notice(message.from, message.target, message.message),
        }
    }
}

fn message_of<'a, S>(prefix: S, command: Command) -> Message
where
    S: Into<String>,
{
    Message {
        tags: None,
        prefix: {
            let p: String = prefix.into();
            // XXX don't compute user from prefix, but use something like
            // matrix id when available?
            let user = p[..min(p.len(), 6)].to_string();
            Some(Prefix::Nickname(p, user, "matrirc".to_string()))
        },
        command,
    }
}

fn message_of_noprefix(command: Command) -> Message {
    Message {
        tags: None,
        prefix: None,
        command,
    }
}

/// msg to client as is without any formatting
pub fn raw_msg<'a, S: Into<String>>(msg: S) -> Message {
    message_of_noprefix(Command::Raw(msg.into(), vec![]))
}

pub fn pong(server: String, server2: Option<String>) -> Message {
    message_of_noprefix(Command::PONG(server, server2))
}

/// privmsg to target, coming as from, with given content.
/// target should be user's nick for private messages or channel name
pub fn privmsg<'a, S, T, U>(from: S, target: T, msg: U) -> Message
where
    S: Into<String>,
    T: Into<String>,
    U: Into<String>,
{
    message_of(from, Command::PRIVMSG(target.into(), msg.into()))
}

pub fn notice<'a, S, T, U>(from: S, target: T, msg: U) -> Message
where
    S: Into<String>,
    T: Into<String>,
    U: Into<String>,
{
    message_of(from, Command::NOTICE(target.into(), msg.into()))
}

pub fn error<'a, S>(reason: S) -> Message
where
    S: Into<String>,
{
    message_of_noprefix(Command::ERROR(reason.into()))
}

pub async fn ircd_sync_write(
    mut writer: SplitSink<Framed<TcpStream, IrcCodec>, Message>,
    mut irc_sink_rx: mpsc::Receiver<Message>,
) -> Result<()> {
    while let Some(message) = irc_sink_rx.recv().await {
        match message.command {
            Command::ERROR(_) => {
                writer.send(message).await?;
                writer.close().await?;
                info!("Stopping write task to quit");
                return Ok(());
            }
            _ => writer.send(message).await?,
        }
    }
    info!("Stopping write task to sink closed");
    Ok(())
}

pub async fn ircd_sync_read(
    mut reader: SplitStream<Framed<TcpStream, IrcCodec>>,
    matrirc: Matrirc,
) -> Result<()> {
    while let Some(message) = reader.try_next().await? {
        trace!("Got message {}", message);
        match message.command {
            Command::PING(server, server2) => matrirc.irc().send(pong(server, server2)).await?,
            Command::PRIVMSG(target, msg) => {
                // parrot for now, send to matrix next
                matrirc
                    .irc()
                    .send_privmsg(target, &matrirc.irc().nick, msg)
                    .await?
            }
            _ => info!("Unhandled message {}", message),
        }
    }
    info!("Stopping read task to stream closed");
    Ok(())
}
