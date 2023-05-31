use anyhow::{Context, Result};
use futures::stream::SplitSink;
use futures::SinkExt;
use irc::proto::IrcCodec;
use irc::{
    client::prelude::{Command, Message, Prefix},
    proto::error::ProtocolError,
};
use log::info;
use std::cmp::min;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

fn message_of<'a, S>(prefix: S, command: Command) -> Message
where
    S: Into<String>,
{
    Message {
        tags: None,
        prefix: {
            let p: String = prefix.into();
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
pub fn raw_msg<'a, S>(msg: S) -> Message
where
    S: Into<String>,
{
    message_of_noprefix(Command::Raw(msg.into(), vec![]))
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

/// only used during login
pub async fn send_raw_msg<'a, S, T>(stream: &mut S, msg: T) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
    T: Into<String>,
{
    stream.send(raw_msg(msg)).await.context("send_raw_msg")
}

pub async fn send_privmsg<'a, S, T, U, V>(stream: &mut S, from: T, target: U, msg: V) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
    T: Into<String>,
    U: Into<String>,
    V: Into<String>,
{
    stream
        .send(privmsg(from, target, msg))
        .await
        .context("send_privmsg")
}

pub async fn irc_write_thread(
    mut writer: SplitSink<Framed<TcpStream, IrcCodec>, Message>,
    mut irc_sink_rx: mpsc::Receiver<Message>,
) -> Result<()> {
    while let Some(message) = irc_sink_rx.recv().await {
        match message.command {
            Command::QUIT(_) => {
                writer.send(message).await?;
                writer.close().await?;
                info!("Stopping write thread to quit");
                return Ok(());
            }
            _ => writer.send(message).await?,
        }
    }
    info!("Stopping write thread to sink closed");
    Ok(())
}
