use anyhow::Result;
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
    /// channel name
    chan: String,
    /// matrix user -> nick for channel.
    /// display names is a per-channel property, so we need to
    /// remember this for each user individually
    members: HashMap<OwnedUserId, String>,
    /// list of irc names in channel
    /// used to enforce unicity, and perhaps later to convert
    /// mentions: to matric mentions
    names: HashMap<String, OwnedUserId>,
}

#[derive(Debug, Clone)]
struct JoiningChan {
    /// The channel
    chan: Chan,
    /// list of pending messages: if someone tries to grab
    /// a chan we're in the process of joining, they should just append
    /// here and the joining task will submit it when it's done.
    /// XXX: If there are any pending messages left when we exit (because e.g. client exited while
    /// we weren't done with join yet), these messages will have been ack'd on matrix side and
    /// won't ever be sent to irc. This should be rare enough but probably worth fixing somehow...
    /// can we just get room member list and stuff synchronously?
    pending_messages: Vec<Message>,
}

#[derive(Debug, Clone)]
struct Query {
    /// query name
    target: String,
    /// used to differentiate ourselves from query target
    /// when we speak on another client
    own_id: OwnedUserId,
}

#[derive(Debug, Clone)]
pub struct RoomTarget {
    /// the Arc/RwLock let us return/modify it without holding the mappings lock
    inner: Arc<RwLock<RoomTargetInner>>,
}

#[derive(Debug, Clone)]
enum RoomTargetInner {
    /// room maps to a query e.g. single other member (or alone!)
    Query(Query),
    /// room maps to a chan, and irc side has it joined
    Chan(Chan),
    /// room maps to a chan, but we're not joined: will force join
    /// on next message or user can join if they want
    LeftChan(Chan),
    /// currently being joined chan, don't rush...
    /// The vec is
    JoiningChan(JoiningChan),
}

#[derive(Default, Debug)]
pub struct Mappings {
    inner: RwLock<MappingsInner>,
}

#[derive(Default, Debug)]
struct MappingsInner {
    /// matrix room id to either chan or query
    rooms: HashMap<OwnedRoomId, RoomTarget>,
    /// chan name to room id, names are registered and reserved even if not joined,
    /// but there can be rooms we haven't seen yet
    /// TODO: add a metacommand to force iterating Matrirc.matrix().rooms() ?
    /// (probably want this to list available query targets too...)
    chans: HashMap<String, Arc<OwnedRoomId>>,
    /// query name to room id; note 'matrirc' and own irc nick are reserved.
    queries: HashMap<String, Arc<OwnedRoomId>>,
}

fn sanitize<'a, S: Into<String>>(str: S) -> String {
    // replace with rust 1.70 OnceCell? eventually
    lazy_static! {
        static ref SANITIZE: Regex = Regex::new("[^a-zA-Z_-]+").unwrap();
    }
    SANITIZE.replace_all(&str.into(), "").into()
}

impl Chan {
    fn new(chan: String) -> Self {
        Chan {
            chan,
            members: HashMap::new(),
            names: HashMap::new(),
        }
    }
    async fn get_member(&self, member_id: &UserId) -> Option<String> {
        self.members.get(member_id).cloned()
    }
}

impl JoiningChan {
    fn new(chan: String) -> Self {
        JoiningChan {
            chan: Chan::new(chan),
            pending_messages: Vec::new(),
        }
    }
}

impl RoomTarget {
    fn query<'a, S: Into<String>>(target: S, own_id: OwnedUserId) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner::Query(Query {
                target: sanitize(target),
                own_id,
            }))),
        }
    }
    fn new_chan<'a, S: Into<String>>(chan_name: S) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner::JoiningChan(JoiningChan::new(
                sanitize(chan_name),
            )))),
        }
    }

    async fn join_chan(&self) -> Result<Vec<Message>> {
        let mut lock = self.inner.write().await;
        // XXX can we "move" this instead of copy (*lock) or clone (get ref &*lock + clone)?
        let (chan, messages) = match &*lock {
            RoomTargetInner::JoiningChan(JoiningChan {
                chan,
                pending_messages,
            }) => (chan, pending_messages.clone()),
            RoomTargetInner::LeftChan(chan) => (chan, Vec::new()),
            _ => return Err(anyhow::Error::msg("invalid room target")),
        };
        *lock = RoomTargetInner::Chan(chan.clone());
        Ok(messages)
    }

    async fn query_of_room(room: &Room) -> Self {
        // XXX: missing dedup, we need to pass mappings in argument
        // so we can check queries and add _x or whatever
        // Also pre-fill 'matrirc' and irc.nick in that map (or
        // add another map for special queries...)
        match room.members(RoomMemberships::ACTIVE).await {
            Ok(members) => {
                let other_member = if room.active_members_count() == 2
                    && members[0].user_id() == room.own_user_id()
                {
                    &members[1]
                } else {
                    &members[0]
                };
                // XXX perhaps don't set our own user id if we're alone,
                // to avoid the from self prefix all the time for a note room?
                return RoomTarget::query(
                    other_member.name().to_string(),
                    room.own_user_id().to_owned(),
                );
            }
            Err(error) => {
                info!("Error getting room members: {}", error);
                // XXX fail harder here? won't be able to track who said what...
            }
        };
        let name = match room.display_name().await {
            Ok(room_name) => room_name.to_string(),
            Err(error) => {
                info!("Error getting room display name: {}", error);
                room.room_id().to_string()
            }
        };
        // feed a fake user id to query so we always prefix messages,
        // as it's not normal we didn't find out who it was.
        RoomTarget::query(name, room.own_user_id().to_owned())
    }

    async fn join_chan_of_room(room: &Room) -> Self {
        let room_name = match room.display_name().await {
            Ok(room_name) => room_name.to_string(),
            Err(error) => {
                info!("Error getting room display name: {}", error);
                // Fallback to the room ID.
                room.room_id().to_string()
            }
        };
        RoomTarget::new_chan(room_name)
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
            RoomTargetInner::Query(target) => IrcMessage {
                message_type,
                from: target.target.clone(),
                target: irc.nick.clone(),
                message: if sender_id == target.own_id {
                    format!("<from self> {}", message)
                } else {
                    message.into()
                },
            },

            // XXX chans are still queries at this point
            RoomTargetInner::Chan(chan) => IrcMessage {
                message_type,
                from: chan.chan.clone(),
                target: irc.nick.clone(),
                message: format!("<from {}> {}", sender_id, message),
            },
            // This one should trigger a join and queue message
            RoomTargetInner::LeftChan(chan) => IrcMessage {
                message_type,
                from: chan.chan.clone(),
                target: irc.nick.clone(),
                message: format!("<from {}> {}", sender_id, message),
            },
            // This one should just queue message
            RoomTargetInner::JoiningChan(jchan) => IrcMessage {
                message_type,
                from: jchan.chan.chan.clone(),
                target: irc.nick.clone(),
                message: format!("<from {}> {}", sender_id, message),
            },
        }
        .into();
        irc.send(message).await
    }
}

impl Mappings {
    pub async fn room_target(&self, room: &Room) -> RoomTarget {
        // happy case first
        if let Some(target) = self.inner.read().await.rooms.get(room.room_id()) {
            return target.clone();
        }
        let target = if room.active_members_count() <= 2 {
            RoomTarget::query_of_room(room).await
        } else {
            RoomTarget::join_chan_of_room(room).await
        };
        // let's try to insert it...
        match self.inner.write().await.rooms.entry(room.room_id().into()) {
            Entry::Occupied(entry) => {
                // got raced, just return whatever we found and throw away our hard work
                return entry.get().clone();
            }
            Entry::Vacant(entry) => entry.insert(target.clone()),
        };
        target
        // XXX get room.active_member_count() to decide chan or query
        // member list with room.members(RoomMemberships::JOIN) only on join
        // if chan: ↓ + XXX get room topic
    }
    async fn join_chan(&self, room: &Room) -> Result<Vec<Message>> {
        let lock = self.inner.read().await;
        let Some(target) = lock.rooms.get(room.room_id()) else {
            return Err(anyhow::Error::msg(format!("Trying to join a room we didn't find: {}", room.room_id())))
        };
        target.join_chan().await
    }
    // XXX promote/demote chans on join/leave events:
    // 1 -> 2 active, check for name/rename query
    // 2 -> 3+, convert from query to chan
    // 3+ -> 3, demote to query?
    // 2 -> 1, rename to avoid confusion?
    async fn user_nick(&self, user_id: &UserId, member: Option<RoomMember>) -> Option<String> {
        // lookup in users list
        // if not fund get display name, ensure unicity through nick_list... or not? display names
        // can be different per room? check this further..
        None
    }
}
