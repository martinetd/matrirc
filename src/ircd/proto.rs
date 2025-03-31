use anyhow::Result;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use irc::client::prelude::{Command, Message, Prefix};
use irc::proto::{ChannelMode, IrcCodec, Mode};
use log::{info, trace, warn};
use matrix_sdk::RoomMemberships;
use std::cmp::min;
use std::time::SystemTime;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::{matrirc::Matrirc, matrix::room_mappings::RoomTargetType, matrix::MatrixMessageType};

/// it's a bit of a pain to redo the work twice for notice/privmsg,
/// so these types wrap it around a bit
#[derive(Debug, Clone)]
pub enum IrcMessageType {
    Privmsg,
    Notice,
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
    pub text: String,
}

impl IntoIterator for IrcMessage {
    type Item = Message;
    // XXX would skip the collect, but cannot return
    // because lifetime: IrcMessage would need to be IrcMessage<'a> with &'a str
    // core::iter::Map<core::str::Split<'_, char>, Self::Item>;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        let IrcMessage {
            text,
            message_type,
            from,
            target,
        } = self;
        text.split('\n')
            .map(|line| match message_type {
                IrcMessageType::Privmsg => privmsg(from.clone(), target.clone(), line),
                IrcMessageType::Notice => notice(from.clone(), target.clone(), line),
            })
            .collect::<Vec<Message>>()
            .into_iter()
    }
}

fn message_of<S>(prefix: S, command: Command) -> Message
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

fn message_of_option<S>(prefix: Option<S>, command: Command) -> Message
where
    S: Into<String>,
{
    match prefix {
        None => message_of_noprefix(command),
        Some(p) => message_of(p, command),
    }
}

/// msg to client as is without any formatting
pub fn raw_msg<S: Into<String>>(msg: S) -> Message {
    message_of_noprefix(Command::Raw(msg.into(), vec![]))
}

pub fn join<S, T>(who: Option<S>, chan: T) -> Message
where
    S: Into<String>,
    T: Into<String>,
{
    message_of_option(who, Command::JOIN(chan.into(), None, None))
}

pub fn part<S, T>(who: Option<S>, chan: T) -> Message
where
    S: Into<String>,
    T: Into<String>,
{
    message_of_option(who, Command::PART(chan.into(), None))
}

pub fn pong(server: String, server2: Option<String>) -> Message {
    message_of_noprefix(Command::PONG(server, server2))
}

/// privmsg to target, coming as from, with given content.
/// target should be user's nick for private messages or channel name
pub fn privmsg<S, T, U>(from: S, target: T, msg: U) -> Message
where
    S: Into<String>,
    T: Into<String>,
    U: Into<String>,
{
    message_of(from, Command::PRIVMSG(target.into(), msg.into()))
}

pub fn notice<S, T, U>(from: S, target: T, msg: U) -> Message
where
    S: Into<String>,
    T: Into<String>,
    U: Into<String>,
{
    message_of(from, Command::NOTICE(target.into(), msg.into()))
}

pub fn error<S>(reason: S) -> Message
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

pub async fn list_channels(target: &str, matrirc: &Matrirc) -> Result<()> {
    let irc = matrirc.irc();
    let matrix = matrirc.matrix();
    let mapping = matrirc.mappings();

    irc.send(raw_msg(format!(
        ":matrirc 321 {} Channel :Users  Name",
        target
    )))
    .await?;
    for joined in matrix.joined_rooms() {
        if joined.is_tombstoned() {
            trace!(
                "Skipping tombstoned {}",
                joined
                    .name()
                    .unwrap_or_else(|| joined.room_id().to_string())
            );
            continue;
        }
        let roomtarget = mapping.try_room_target(&joined).await?;
        let chantype = roomtarget.target_type().await;
        let channame = roomtarget.target().await;
        if chantype != RoomTargetType::Query {
            let users = joined.members_no_sync(RoomMemberships::ACTIVE).await?.len();
            let topic = joined.topic().unwrap_or_default();
            irc.send(raw_msg(format!(
                ":matrirc 322 {} #{} {} :{}",
                target, channame, users, topic
            )))
            .await?;
        }
    }
    irc.send(raw_msg(format!(":matrirc 323 {} :End of /LIST", target)))
        .await?;
    Ok(())
}

pub async fn ircd_sync_read(
    mut reader: SplitStream<Framed<TcpStream, IrcCodec>>,
    matrirc: Matrirc,
) -> Result<()> {
    while let Some(input) = reader.next().await {
        let message = match input {
            Err(e) => {
                info!("Ignoring error message {:?}", e);
                continue;
            }
            Ok(m) => m,
        };
        trace!("Got message {}", message);
        match message.command.clone() {
            Command::PING(server, server2) => matrirc.irc().send(pong(server, server2)).await?,
            Command::PRIVMSG(target, msg) => {
                let (message_type, msg) = if let Some(emote) = msg.strip_prefix("\u{001}ACTION ") {
                    (MatrixMessageType::Emote, emote.to_string())
                } else {
                    (MatrixMessageType::Text, msg)
                };
                if let Err(e) = matrirc
                    .mappings()
                    .to_matrix(&target, message_type, msg)
                    .await
                {
                    warn!("Could not forward message: {:?}", e);
                    if let Err(e2) = matrirc
                        .irc()
                        .send(notice(
                            &matrirc.irc().nick,
                            message.response_target().unwrap_or("matrirc"),
                            format!("Could not forward: {}", e),
                        ))
                        .await
                    {
                        warn!("Furthermore, reply errored too: {:?}", e2);
                    }
                }
            }
            Command::NOTICE(target, msg) => {
                if let Err(e) = matrirc
                    .mappings()
                    .to_matrix(&target, MatrixMessageType::Notice, msg)
                    .await
                {
                    warn!("Could not forward message: {:?}", e);
                    if let Err(e2) = matrirc
                        .irc()
                        .send(notice(
                            &matrirc.irc().nick,
                            message.response_target().unwrap_or("matrirc"),
                            format!("Could not forward: {}", e),
                        ))
                        .await
                    {
                        warn!("Furthermore, reply errored too: {:?}", e2);
                    }
                }
            }
            Command::ChannelMODE(chan, modes) if modes.is_empty() => {
                if let Err(e) = matrirc
                    .irc()
                    .send(raw_msg(format!(
                        ":matrirc 329 {} {} {}",
                        matrirc.irc().nick,
                        chan,
                        // normally chan creation timestamp
                        SystemTime::now()
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or_default()
                    )))
                    .await
                {
                    warn!("Could not reply to mode: {:?}", e)
                }
            }
            Command::ChannelMODE(chan, modes)
                if modes.contains(&Mode::NoPrefix(ChannelMode::Ban)) =>
            {
                if let Err(e) = matrirc
                    .irc()
                    .send(raw_msg(format!(
                        ":matrirc 368 {} {} :End",
                        matrirc.irc().nick,
                        chan
                    )))
                    .await
                {
                    warn!("Could not reply to mode: {:?}", e)
                }
            }
            Command::WHO(Some(chan), _) => {
                if let Err(e) = matrirc
                    .irc()
                    .send(raw_msg(format!(
                        ":matrirc 315 {} {} :End",
                        matrirc.irc().nick,
                        chan
                    )))
                    .await
                {
                    warn!("Could not reply to mode: {:?}", e)
                }
            }
            Command::LIST(_, _) => {
                if let Err(e) =
                    list_channels(message.response_target().unwrap_or("matrirc"), &matrirc).await
                {
                    warn!("Could not list channels: {:?}", e)
                }
            }
            _ => info!("Unhandled message {:?}", message),
        }
    }
    info!("Stopping read task to stream closed");
    Ok(())
}
