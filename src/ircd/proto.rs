use anyhow::{Context, Result};
use futures::SinkExt;
use irc::{
    client::prelude::{Command, Message, Prefix},
    proto::error::ProtocolError,
};
use std::cmp::min;

/// Wrapper to send command to given stream
async fn send_cmd<'a, S, T>(stream: &mut S, prefix: Option<T>, command: Command) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
    T: Into<String>,
{
    stream
        .send(Message {
            tags: None,
            prefix: prefix.and_then(|p| {
                let s: String = p.into();
                let user = s[..min(s.len(), 6)].to_string();
                Some(Prefix::Nickname(s, user, "matrirc".to_string()))
            }),
            command,
        })
        .await
        .context("send_cmd")
}

/// Wrapper for send_cmd just to cast None as some specific
/// option type (required because async functions embed their types)
async fn send_cmd_noprefix<'a, S>(stream: &mut S, command: Command) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
{
    send_cmd(stream, None as Option<String>, command).await
}

/// send msg to client as is without any formatting
pub async fn send_raw_msg<'a, S, T>(stream: &mut S, msg: T) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
    T: Into<String>,
{
    send_cmd_noprefix(stream, Command::Raw(msg.into(), vec![]))
        .await
        .context("send_raw_msg")
}

/// send privmsg to target, coming as from, with given content.
/// target should be user's nick for private messages or channel name
pub async fn send_privmsg<'a, S, T>(stream: &mut S, from: T, target: T, msg: T) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
    T: Into<String>,
{
    send_cmd(
        stream,
        Some(from),
        Command::PRIVMSG(target.into(), msg.into()),
    )
    .await
    .context("send_privmsg")
}
