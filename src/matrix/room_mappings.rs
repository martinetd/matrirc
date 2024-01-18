use anyhow::{Error, Result};
use async_trait::async_trait;
use lazy_static::lazy_static;
use log::{trace, warn};
use matrix_sdk::{
    room::Room,
    ruma::{OwnedRoomId, OwnedUserId},
    RoomMemberships,
};
use regex::Regex;
use std::borrow::Cow;
use std::collections::{
    hash_map::{Entry, HashMap},
    VecDeque,
};
use std::sync::Arc;
use tokio::sync::{RwLock, RwLockWriteGuard};

use crate::ircd;
use crate::ircd::{
    join_irc_chan, join_irc_chan_finish,
    proto::{IrcMessage, IrcMessageType},
    IrcClient,
};
use crate::matrirc::Matrirc;

pub enum MatrixMessageType {
    Text,
    Emote,
    Notice,
}

#[derive(Debug, Clone)]
struct TargetMessage {
    /// privmsg or notice
    message_type: IrcMessageType,
    /// will be either from in channel, or added as prefix if different from query name
    from: String,
    /// actual message
    text: String,
}

impl TargetMessage {
    fn new(message_type: IrcMessageType, from: String, text: String) -> Self {
        TargetMessage {
            message_type,
            from,
            text,
        }
    }
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
    LeftChan,
    /// Join in progress
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
    members: HashMap<String, String>,
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

pub struct Mappings {
    inner: RwLock<MappingsInner>,
    pub irc: IrcClient,
    mt: RoomTarget,
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
    async fn set_target(&self, target: RoomTarget);
}

fn sanitize<S: Into<String>>(str: S) -> String {
    // replace with rust 1.70 OnceCell? eventually
    lazy_static! {
        static ref SANITIZE: Regex = Regex::new("[^a-zA-Z_-]+").unwrap();
    }
    SANITIZE.replace_all(&str.into(), "").into()
}

pub async fn room_name(room: &matrix_sdk::BaseRoom) -> String {
    if let Ok(name) = room.display_name().await {
        return name.to_string();
    }
    if let Some(name) = room.name() {
        return name.to_string();
    }
    room.room_id().to_string()
}

trait InsertDedup<V> {
    fn insert_deduped(&mut self, orig_key: &str, value: V) -> String;
}

impl<V> InsertDedup<V> for HashMap<String, V> {
    fn insert_deduped(&mut self, orig_key: &str, value: V) -> String {
        let mut key: String = orig_key.to_string();
        let mut count = 1;
        loop {
            if let Entry::Vacant(entry) = self.entry(key) {
                let found = entry.key().clone();
                entry.insert(value);
                return found;
            }
            count += 1;
            key = format!("{}_{}", orig_key, count);
        }
    }
}

async fn fill_room_members(
    mut target_lock: RwLockWriteGuard<'_, RoomTargetInner>,
    room: Room,
    room_name: String,
) -> Result<()> {
    let members = room.members(RoomMemberships::ACTIVE).await?;
    match members.len() {
        0 => {
            // XXX remove room from mappings, but this should never happen anyway
            return Err(Error::msg(format!("Message in empty room {}?", room_name)));
        }
        // promote to chan if other member name isn't room name
        1 | 2 => {
            if members.iter().all(|m| m.name() != room_name) {
                target_lock.target_type = RoomTargetType::LeftChan;
            }
        }
        _ => target_lock.target_type = RoomTargetType::LeftChan,
    }
    for member in members {
        // XXX qol improvement: rename own user id to irc.nick
        // ensure we preseve room target's name to simplify member's nick in queries
        let member_name = match member.name() {
            n if n == room_name => target_lock.target.clone(),
            n => sanitize(n),
        };
        let name = target_lock
            .names
            .insert_deduped(&member_name, member.user_id().to_owned());
        target_lock.members.insert(member.user_id().into(), name);
    }
    Ok(())
}

impl RoomTarget {
    fn new<S: Into<String>>(target_type: RoomTargetType, target: S) -> Self {
        RoomTarget {
            inner: Arc::new(RwLock::new(RoomTargetInner {
                target: target.into(),
                target_type,
                members: HashMap::new(),
                names: HashMap::new(),
                pending_messages: RwLock::new(VecDeque::new()),
            })),
        }
    }
    fn query<S: Into<String>>(target: S) -> Self {
        RoomTarget::new(RoomTargetType::Query, target)
    }
    pub async fn target(&self) -> String {
        self.inner.read().await.target.clone()
    }

    async fn join_chan(&self, irc: &IrcClient) -> bool {
        let mut lock = self.inner.write().await;
        match &lock.target_type {
            RoomTargetType::LeftChan => (),
            RoomTargetType::Query => (),
            // got raced or already joined
            RoomTargetType::JoiningChan => return false,
            RoomTargetType::Chan => return false,
        };
        lock.target_type = RoomTargetType::JoiningChan;
        let chan = format!("#{}", lock.target);
        drop(lock);

        // we need to initate the join before getting members in another task
        if let Err(e) = join_irc_chan(irc, &chan).await {
            warn!("Could not join irc: {e}");
            // XXX send message to irc through matrirc query
            return false;
        }
        let target = self.clone();
        let irc = irc.clone();
        tokio::spawn(async move {
            let names_list = target.names_list().await;
            if let Err(e) = join_irc_chan_finish(&irc, chan, names_list).await {
                warn!("Could not join irc: {e}");
                // XXX send message to irc through matrirc query
                return;
            }
            if let Err(e) = target.finish_join(&irc).await {
                warn!("Could not finish join: {e}");
                // XXX irc message
            }
        });
        true
    }

    async fn names_list(&self) -> Vec<String> {
        // need to clone because of lock -- could do better?
        self.inner.read().await.names.keys().cloned().collect()
    }

    async fn finish_join(&self, irc: &IrcClient) -> Result<()> {
        self.flush_pending_messages(irc).await?;
        self.inner.write().await.target_type = RoomTargetType::Chan;
        // recheck in case some new message was stashed before we got write lock
        self.flush_pending_messages(irc).await?;
        Ok(())
    }

    pub async fn member_join(
        &self,
        irc: &IrcClient,
        member: OwnedUserId,
        name: Option<String>,
    ) -> Result<()> {
        let mut guard = self.inner.write().await;
        let chan = format!("#{}", guard.target);
        trace!("{:?} ({}) joined {}", name, member, chan);
        // XXX wait a bit and list room members if name is none?
        let name = sanitize(name.unwrap_or_else(|| member.to_string()));
        let name = guard.names.insert_deduped(&name, member.clone());
        guard.members.insert(member.into(), name.clone());
        drop(guard);
        if !self.join_chan(irc).await {
            // already joined chan, send join to irc
            irc.send(ircd::proto::join(Some(name), chan)).await?;
        }
        Ok(())
    }

    pub async fn member_part(&self, irc: &IrcClient, member: OwnedUserId) -> Result<()> {
        let mut guard = self.inner.write().await;
        let Some(name) = guard.members.remove(member.as_str()) else {
            // not in chan
            return Ok(());
        };
        let chan = format!("#{}", guard.target);
        trace!("{:?} ({}) part {}", name, member, chan);
        let _ = guard.names.remove(&name);
        drop(guard);
        irc.send(ircd::proto::part(Some(name), chan)).await?;
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
            .push_back(TargetMessage::new(
                IrcMessageType::Notice,
                "matrirc".to_string(),
                error,
            ));
        self
    }

    async fn target_message_to_irc(&self, irc: &IrcClient, message: TargetMessage) -> IrcMessage {
        match &*self.inner.read().await {
            RoomTargetInner {
                target,
                target_type: RoomTargetType::Query,
                ..
            } => IrcMessage {
                message_type: message.message_type,
                from: target.clone(),
                target: irc.nick.clone(),
                text: if &message.from == target {
                    message.text
                } else {
                    format!("<{}> {}", message.from, message.text)
                },
            },
            // mostly normal chan, but finish_join can also use ths on JoningChan
            // we could error on LeftChan but what's the point?
            RoomTargetInner { target, .. } => IrcMessage {
                message_type: message.message_type,
                from: message.from,
                target: format!("#{}", target),
                text: message.text,
            },
        }
    }

    pub async fn flush_pending_messages(&self, irc: &IrcClient) -> Result<()> {
        let inner = self.inner.read().await;
        if !inner.pending_messages.read().await.is_empty() {
            while let Some(target_message) = inner.pending_messages.write().await.pop_front() {
                for irc_message in self.target_message_to_irc(irc, target_message).await {
                    irc.send(irc_message).await?
                }
            }
        };
        Ok(())
    }

    pub async fn send_text_to_irc<'a, S>(
        &self,
        irc: &IrcClient,
        message_type: IrcMessageType,
        sender: &String,
        text: S,
    ) -> Result<()>
    where
        S: Into<String>,
    {
        let inner = self.inner.read().await;
        let message = TargetMessage {
            message_type,
            from: inner
                .members
                .get(sender)
                .map(Cow::Borrowed)
                .unwrap_or_else(|| Cow::Owned(sender.clone()))
                .to_string(),
            text: text.into(),
        };
        match inner.target_type {
            RoomTargetType::LeftChan => {
                trace!("Queueing message and joining chan");
                inner.pending_messages.write().await.push_back(message);
                drop(inner);
                self.join_chan(irc).await;
                return Ok(());
            }
            RoomTargetType::JoiningChan => {
                trace!("Queueing message (join in progress)");
                inner.pending_messages.write().await.push_back(message);
                return Ok(());
            }
            _ => (),
        }
        drop(inner);

        // really send -- start with pending messages if any
        self.flush_pending_messages(irc).await?;

        for irc_message in self.target_message_to_irc(irc, message).await {
            irc.send(irc_message).await?
        }
        Ok(())
    }
    pub async fn send_simple_query<S>(&self, irc: &IrcClient, text: S) -> Result<()>
    where
        S: Into<String>,
    {
        self.send_text_to_irc(irc, IrcMessageType::Privmsg, &self.target().await, text)
            .await
    }
}

impl Mappings {
    pub fn new(irc: IrcClient) -> Self {
        Mappings {
            inner: MappingsInner::default().into(),
            irc,
            mt: RoomTarget::query("matrirc"),
        }
    }
    pub async fn room_target(&self, room: &Room) -> RoomTarget {
        match self.try_room_target(room).await {
            Ok(target) => target,
            Err(e) => {
                // return error into matrirc channel instead
                self.mt
                    .clone()
                    .set_error(format!("Could not find or create target: {}", e))
                    .await
            }
        }
    }
    pub async fn matrirc_query<S>(&self, message: S) -> Result<()>
    where
        S: Into<String>,
    {
        self.mt.send_simple_query(&self.irc, message).await
    }

    pub async fn insert_deduped(
        &self,
        candidate: &str,
        target: &(impl MessageHandler + Send + Sync + Clone + 'static),
    ) -> RoomTarget {
        let mut guard = self.inner.write().await;
        let name = guard
            .targets
            .insert_deduped(candidate, Box::new(target.clone()));
        let room_target = RoomTarget::query(name);
        target.set_target(room_target.clone()).await;
        room_target
    }

    pub async fn remove_target(&self, name: &str) {
        self.inner.write().await.targets.remove(name);
    }

    // note this cannot use insert_free_target because we want to keep write lock
    // long enough to check for deduplicate and it's a bit of a mess; it could be done
    // with a more generic 'insert_free_target' that takes a couple of callbacks but
    // it's just not worth it
    async fn try_room_target(&self, room: &Room) -> Result<RoomTarget> {
        // happy case first
        if let Some(target) = self.inner.read().await.rooms.get(room.room_id()) {
            return Ok(target.clone());
        }

        // create a new and try to insert it...
        let desired_name = sanitize(room_name(room).await);

        // lock mappings and insert into hashs
        let mut mappings = self.inner.write().await;
        if let Some(target) = mappings.rooms.get(room.room_id()) {
            // got raced
            return Ok(target.clone());
        }
        // find unique irc name
        let name = mappings
            .targets
            .insert_deduped(&desired_name, Box::new(room.clone()));
        trace!("Creating room {}", name);
        // create a query anyway, we'll promote it when we get members
        let target = RoomTarget::query(&name);
        mappings.rooms.insert(room.room_id().into(), target.clone());

        // lock target and release mapping lock we no longer need
        let target_lock = target.inner.write().await;
        drop(mappings);

        let room_clone = room.clone();
        // XXX do this in a tokio::spawn task:
        // can't seem to pass target_lock as its lifetime depends on target (or
        // its clone), but we can't pass target and target lock because target can't be used while
        // target_lock is alive...
        fill_room_members(target_lock, room_clone, desired_name).await?;
        Ok(target)
    }

    pub async fn to_matrix(
        &self,
        name: &str,
        message_type: MatrixMessageType,
        message: String,
    ) -> Result<()> {
        let name = match name.strip_prefix('#') {
            Some(suffix) => suffix,
            None => name,
        };
        if let Some(target) = self.inner.read().await.targets.get(name) {
            target.handle_message(message_type, message).await
        } else {
            Err(Error::msg(format!("No such target {}", name)))
        }
    }

    pub async fn sync_rooms(&self, matrirc: &Matrirc) -> Result<()> {
        let client = matrirc.matrix();
        for joined in client.joined_rooms() {
            if joined.is_tombstoned() {
                trace!(
                    "Skipping tombstoned {}",
                    joined
                        .name()
                        .unwrap_or_else(|| joined.room_id().to_string())
                );
                continue;
            }
            self.try_room_target(&Room::Joined(joined)).await?;
        }
        self.matrirc_query("Finished initial room sync").await?;
        Ok(())
    }
}
