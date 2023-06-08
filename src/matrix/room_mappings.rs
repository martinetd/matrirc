use anyhow::{Error, Result};
use irc::client::prelude::{Command, Message};
use lazy_static::lazy_static;
use log::info;
use matrix_sdk::{
    room::{Room, RoomMember},
    ruma::user_id,
    ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId},
    RoomMemberships,
};
use regex::Regex;
use std::borrow::Cow;
use std::collections::hash_map::{Entry, HashMap};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, RwLockReadGuard};

use crate::ircd::{
    proto::{IrcMessage, IrcMessageType},
    IrcClient,
};
use crate::matrirc::Matrirc;

#[derive(Debug, Clone)]
struct Chan {
    /// channel name or query target
    target: String,
    /// matrix user -> nick for channel.
    /// display names is a per-channel property, so we need to
    /// remember this for each user individually.
    /// In queries case, any non-trivial member is expanded as <nick> at
    /// the start of the message
    members: HashMap<OwnedUserId, String>,
    /// list of irc names in channel
    /// used to enforce unicity, and perhaps later to convert
    /// `mentions:` to matric mentions
    names: HashMap<String, OwnedUserId>,
    /// used for error messages, and to queue messages in joinin chan:
    /// if someone tries to grab a chan we're currently joining they just
    /// append to it instead of sending message to irc
    /// XXX: If there are any pending messages left when we exit (because e.g. client exited while
    /// we weren't done with join yet), these messages will have been ack'd on matrix side and
    /// won't ever be sent to irc. This should be rare enough but probably worth fixing somehow...
    pending_messages: Vec<IrcMessage>,
}

#[derive(Debug, Clone)]
pub struct RoomTarget {
    /// the Arc/RwLock let us return/modify it without holding the mappings lock
    inner: Arc<RwLock<RoomTargetInner>>,
}

#[derive(Debug, Clone)]
enum RoomTargetType {
    /// room maps to a query e.g. single other member (or alone!)
    Query,
    /// room maps to a chan, and irc side has it joined
    Chan,
    /// room maps to a chan, but we're not joined: will force join
    /// on next message or user can join if they want
    LeftChan,
    /// Join in progress
    JoiningChan,
}

#[derive(Debug, Clone)]
struct RoomTargetInner {
    target_type: RoomTargetType,
    chan: Chan,
}

#[derive(Default, Debug)]
pub struct Mappings {
    inner: RwLock<MappingsInner>,
}

#[derive(Default, Debug)]
struct MappingsInner {
    /// matrix room id to either chan or query
    rooms: HashMap<OwnedRoomId, RoomTarget>,
    /// chan/query name to room id,
    /// channel names are registered and reserved even if not joined,
    /// but there can be rooms we haven't seen yet
    /// TODO: add a metacommand to force iterating Matrirc.matrix().rooms() ?
    /// (probably want this to list available query targets too...)
    /// TODO: also reserve 'matrirc', irc.nick()...
    targets: HashMap<String, Arc<OwnedRoomId>>,
}

fn sanitize<'a, S: Into<String>>(str: S) -> String {
    // replace with rust 1.70 OnceCell? eventually
    lazy_static! {
        static ref SANITIZE: Regex = Regex::new("[^a-zA-Z_-]+").unwrap();
    }
    SANITIZE.replace_all(&str.into(), "").into()
}

impl Chan {
    fn new<'a, S: Into<String>>(target: S) -> Self {
        Chan {
            target: sanitize(target),
            members: HashMap::new(),
            names: HashMap::new(),
            pending_messages: Vec::new(),
        }
    }
    async fn get_member(&self, member_id: &UserId) -> Option<String> {
        self.members.get(member_id).cloned()
    }
}

impl RoomTarget {
    fn query<'a, S: Into<String>>(target: S) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner {
                target_type: RoomTargetType::Query,
                chan: Chan::new(target),
            })),
        }
    }
    fn chan<'a, S: Into<String>>(chan_name: S) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner {
                target_type: RoomTargetType::LeftChan,
                chan: Chan::new(chan_name),
            })),
        }
    }

    async fn join_chan(&self) -> Result<()> {
        let mut lock = self.inner.write().await;
        match &lock.target_type {
            RoomTargetType::JoiningChan => (),
            RoomTargetType::LeftChan => (),
            _ => return Err(anyhow::Error::msg("invalid room target")),
        };
        lock.target_type = RoomTargetType::Chan;
        Ok(())
    }

    fn set_error(mut self, error: String) -> Self {
        // XXX unpack chan and add to messages
        self
    }

    async fn target_of_room(name: String, room: &Room) -> Result<(Self, Vec<RoomMember>)> {
        // XXX we don't want this to be long: figure out active_members_count
        // https://github.com/matrix-org/matrix-rust-sdk/issues/2010
        let members = room.members(RoomMemberships::ACTIVE).await?;
        match members.len() {
            0 => Err(Error::msg(format!("Message in empty room {}?", name))),
            1 | 2 => Ok((RoomTarget::query(name), members)),
            _ => Ok((RoomTarget::chan(name), members)),
        }
    }

    pub async fn send_irc_message<'a, S>(
        &self,
        irc: &IrcClient,
        message_type: IrcMessageType,
        sender_id: &UserId,
        message: S,
    ) -> Result<()>
    where
        S: Into<String> + std::fmt::Display,
    {
        let message: Message = match &*self.inner.read().await {
            RoomTargetInner {
                target_type: RoomTargetType::Query,
                chan,
            } => IrcMessage {
                message_type,
                from: chan.target.clone(),
                target: irc.nick.clone(),
                message: if let Some(nick) = chan.members.get(sender_id) {
                    format!("<{}> {}", nick, message)
                } else {
                    message.into()
                },
            },

            // XXX chans are still queries at this point
            RoomTargetInner {
                target_type: RoomTargetType::Chan,
                chan,
            } => IrcMessage {
                message_type,
                from: chan.target.clone(),
                target: irc.nick.clone(),
                message: format!(
                    "<{}> {}",
                    chan.members
                        .get(sender_id)
                        .map(Cow::Borrowed)
                        .unwrap_or_else(|| Cow::Owned(sender_id.to_string())),
                    message
                ),
            },

            // This one should trigger a join and queue message
            RoomTargetInner {
                target_type: RoomTargetType::LeftChan,
                chan,
            } => IrcMessage {
                message_type,
                from: chan.target.clone(),
                target: irc.nick.clone(),
                message: format!(
                    "<{}> {}",
                    chan.members
                        .get(sender_id)
                        .map(Cow::Borrowed)
                        .unwrap_or_else(|| Cow::Owned(sender_id.to_string())),
                    message
                ),
            },

            // This one is currently joining, but not there yet: just queue.
            RoomTargetInner {
                target_type: RoomTargetType::JoiningChan,
                chan,
            } => IrcMessage {
                message_type,
                from: chan.target.clone(),
                target: irc.nick.clone(),
                message: format!(
                    "<{}> {}",
                    chan.members
                        .get(sender_id)
                        .map(Cow::Borrowed)
                        .unwrap_or_else(|| Cow::Owned(sender_id.to_string())),
                    message
                ),
            },
        }
        .into();
        irc.send(message).await
    }
}

impl Mappings {
    pub async fn room_target(&self, room: &Room) -> RoomTarget {
        match self.try_room_target(room).await {
            Ok(target) => target,
            Err(e) => {
                // return temporary error channel
                RoomTarget::query("matrirc")
                    .set_error(format!("Could not find or create target: {}", e))
            }
        }
    }
    async fn try_room_target(&self, room: &Room) -> Result<RoomTarget> {
        // happy case first
        if let Some(target) = self.inner.read().await.rooms.get(room.room_id()) {
            return Ok(target.clone());
        }

        // create a new and try to insert it...
        let name = match room.display_name().await {
            Ok(room_name) => room_name.to_string(),
            Err(error) => {
                info!("Error getting room display name: {}", error);
                room.room_id().to_string()
            }
        };

        // lock mappings and insert into hashs
        let mut mappings = self.inner.write().await;
        if let Some(target) = mappings.rooms.get(room.room_id()) {
            // got raced
            return Ok(target.clone());
        }
        // XXX check dups here
        let (target, members) = RoomTarget::target_of_room(name.clone(), room).await?;
        mappings.rooms.insert(room.room_id().into(), target.clone());
        mappings
            .targets
            .insert(name, room.room_id().to_owned().into());

        // lock target and release mapping lock we no longer need
        let mut target_lock = target.inner.write().await;
        drop(mappings);
        for member in members {
            let name = member.name().to_string();
            // XXX skip main target for query (e.g. roomname == membername)
            // XXX dedup
            target_lock
                .chan
                .names
                .insert(name.clone(), member.user_id().to_owned());
            target_lock
                .chan
                .members
                .insert(member.user_id().to_owned(), name);
        }
        // XXX: start task to start join process (needs irc...)
        // drop lock explicitly to allow returning target
        drop(target_lock);
        Ok(target)
    }
    // XXX promote/demote chans on join/leave events:
    // 1 -> 2 active, check for name/rename query
    // 2 -> 3+, convert from query to chan
    // 3+ -> 3, demote to query?
    // 2 -> 1, rename to avoid confusion?
    // XXX update room mappings on join/leave events...
}