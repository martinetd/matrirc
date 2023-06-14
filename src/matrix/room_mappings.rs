use anyhow::{Error, Result};
use async_trait::async_trait;
use irc::client::prelude::Message;
use lazy_static::lazy_static;
use log::{info, trace};
use matrix_sdk::{
    room::{Room, RoomMember},
    ruma::{OwnedRoomId, OwnedUserId, UserId},
    RoomMemberships,
};
use regex::Regex;
use std::borrow::Cow;
use std::collections::{
    hash_map::{Entry, HashMap},
    VecDeque,
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{
    ircd::{
        proto::{IrcMessage, IrcMessageType},
        IrcClient,
    },
    matrix::proto::MatrixMessageType,
};

#[derive(Debug, Clone)]
struct TargetMessage {
    /// privmsg or notice
    message_type: IrcMessageType,
    /// will be either from in channel, or added as prefix if different from query name
    from: String,
    /// actual message
    message: String,
}

#[derive(Debug, Clone)]
pub struct RoomTarget {
    /// the Arc/RwLock let us return/modify it without holding the mappings lock
    inner: Arc<RwLock<RoomTargetInner>>,
}

#[derive(Debug, PartialEq)]
enum RoomTargetType {
    /// room maps to a query e.g. single other member (or alone!)
    Query,
    /// room maps to a chan, and irc side has it joined
    Chan,
    /// room maps to a chan, but we're not joined: will force join
    /// on next message or user can join if they want
    #[allow(unused)]
    LeftChan,
    /// Join in progress
    #[allow(unused)]
    JoiningChan,
}

#[derive(Debug)]
struct RoomTargetInner {
    /// channel name or query target
    target: String,
    /// query, channel joined, or...
    target_type: RoomTargetType,
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
    /// append to it instead of sending message to irc -- it needs its own lock
    /// because we'll modify it while holding read lock on room target (to get target type)
    /// XXX: If there are any pending messages left when we exit (because e.g. client exited while
    /// we weren't done with join yet), these messages will have been ack'd on matrix side and
    /// won't ever be sent to irc. This should be rare enough but probably worth fixing somehow...
    pending_messages: RwLock<VecDeque<TargetMessage>>,
}

#[derive(Default)]
pub struct Mappings {
    inner: RwLock<MappingsInner>,
}

#[derive(Default)]
struct MappingsInner {
    /// matrix room id to either chan or query
    rooms: HashMap<OwnedRoomId, RoomTarget>,
    /// chan/query name to something that'll eat our message.
    /// For matrix rooms, it'll just send to the room as appropriate.
    ///
    /// Note that since we might want to promote/demote chans to query,
    /// targets does NOT include the hash: foobar = #foobar as far as
    /// dedup and received (irc -> matrirc) messages go
    /// TODO: add a metacommand to force iterating Matrirc.matrix().rooms() ?
    /// (probably want this to list available query targets too...)
    /// TODO: also reserve 'matrirc', irc.nick()...
    targets: HashMap<String, Box<dyn MessageHandler + Send + Sync>>,
}

#[async_trait]
pub trait MessageHandler {
    async fn handle_message(&self, message_type: MatrixMessageType, message: String) -> Result<()>;
}

fn sanitize<'a, S: Into<String>>(str: S) -> String {
    // replace with rust 1.70 OnceCell? eventually
    lazy_static! {
        static ref SANITIZE: Regex = Regex::new("[^a-zA-Z_-]+").unwrap();
    }
    SANITIZE.replace_all(&str.into(), "").into()
}

trait InsertDedup<V> {
    fn insert_deduped(&mut self, orig_key: String, value: V) -> String;
}

impl<V> InsertDedup<V> for HashMap<String, V> {
    fn insert_deduped(&mut self, orig_key: String, value: V) -> String {
        let mut key = orig_key.clone();
        let mut count = 1;
        loop {
            match self.entry(key) {
                Entry::Vacant(entry) => {
                    let found = entry.key().clone();
                    entry.insert(value);
                    return found;
                }
                _ => {}
            }
            count += 1;
            key = format!("{}_{}", orig_key, count);
        }
    }
}

impl TargetMessage {
    async fn into_irc_message(self, irc: &IrcClient, target: &RoomTarget) -> IrcMessage {
        match &*target.inner.read().await {
            RoomTargetInner {
                target,
                /* XXX decomment when channels done
                 * target_type: RoomTargetType::Query, */
                ..
            } => IrcMessage {
                message_type: self.message_type,
                from: target.clone(),
                target: irc.nick.clone(),
                message: if &self.from == target {
                    self.message
                } else {
                    format!("<{}> {}", self.from, self.message)
                },
            },
            /*
            // only makes sense if it's joined, check and fallback to query?
            RoomTargetInner { target, .. } => IrcMessage {
                message_type: self.message_type,
                from: self.from,
                target: format!("#{}", target),
                message: self.message,
            },
            */
        }
    }
}

impl RoomTarget {
    fn new<'a, S: Into<String>>(target_type: RoomTargetType, target: S) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner {
                target: sanitize(target),
                target_type,
                members: HashMap::new(),
                names: HashMap::new(),
                pending_messages: RwLock::new(VecDeque::new()),
            })),
        }
    }
    fn query<'a, S: Into<String>>(target: S) -> Self {
        RoomTarget::new(RoomTargetType::Query, target)
    }
    fn chan<'a, S: Into<String>>(chan_name: S) -> Self {
        // XXX create as LeftChan on join on first message
        RoomTarget::new(RoomTargetType::Chan, chan_name)
    }

    #[allow(unused)]
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

    /// error will be sent next time a message from channel is sent
    /// (or when it's finished joining in case of chan trying to join)
    async fn set_error(self, error: String) -> Self {
        self.inner
            .read()
            .await
            .pending_messages
            .write()
            .await
            .push_back(TargetMessage {
                message_type: IrcMessageType::NOTICE,
                from: "matrirc".to_string(),
                message: error,
            });
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

    pub async fn send_to_irc<'a, S>(
        &self,
        irc: &IrcClient,
        message_type: IrcMessageType,
        sender_id: &UserId,
        message: S,
    ) -> Result<()>
    where
        S: Into<String> + std::fmt::Display,
    {
        let inner = self.inner.read().await;
        let message = TargetMessage {
            message_type,
            from: inner
                .members
                .get(sender_id)
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(sender_id.to_string()))
                .to_string(),
            message: message.into(),
        };
        match inner.target_type {
            RoomTargetType::LeftChan => {
                inner.pending_messages.write().await.push_back(message);
                // XXX start join
                return Ok(());
            }
            RoomTargetType::JoiningChan => {
                inner.pending_messages.write().await.push_back(message);
                return Ok(());
            }
            _ => (),
        }

        // really send -- start with pending messages if any
        if !inner.pending_messages.read().await.is_empty() {
            while let Some(target_message) = inner.pending_messages.write().await.pop_front() {
                let message: Message = target_message.into_irc_message(irc, self).await.into();
                irc.send(message).await?
            }
        }

        drop(inner);
        let irc_message = message.into_irc_message(irc, self).await.into();
        irc.send(irc_message).await
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
                    .await
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
        // find unique irc name
        let name = mappings
            .targets
            .insert_deduped(name, Box::new(room.clone()));
        trace!("Creating room {}", name);
        let (target, members) = RoomTarget::target_of_room(name.clone(), room).await?;
        mappings.rooms.insert(room.room_id().into(), target.clone());

        // lock target and release mapping lock we no longer need
        let mut target_lock = target.inner.write().await;
        drop(mappings);
        for member in members {
            let name = target_lock
                .names
                .insert_deduped(member.name().to_string(), member.user_id().to_owned());
            target_lock
                .members
                .insert(member.user_id().to_owned(), name);
        }
        // XXX: start task to start join process (needs irc...)
        // drop lock explicitly to allow returning target
        drop(target_lock);
        Ok(target)
    }
    pub async fn to_matrix(
        &self,
        name: &str,
        message_type: MatrixMessageType,
        message: String,
    ) -> Result<()> {
        // XXX strip leading #
        if let Some(target) = self.inner.read().await.targets.get(name) {
            target.handle_message(message_type, message).await
        } else {
            Err(Error::msg(format!("No such target {}", name)))
        }
    }
    // XXX promote/demote chans on join/leave events:
    // 1 -> 2 active, check for name/rename query
    // 2 -> 3+, convert from query to chan
    // 3+ -> 3, demote to query?
    // 2 -> 1, rename to avoid confusion?
    // XXX update room mappings on join/leave events...
}
