#![type_length_limit="2048000"]

extern crate env_logger;
extern crate anyhow;
#[macro_use]
extern crate log;
extern crate irc;
extern crate tokio;
extern crate tokio_util;
extern crate dialoguer;
extern crate chrono;

use anyhow::{Result, Context, Error};
use tokio::sync::mpsc;
use std::collections::hash_map::{HashMap, Entry};
use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};

use chrono::offset::Local;
use chrono::DateTime;
use std::time::SystemTime;

// ircd stuff
use irc::client::prelude::*;
use irc::proto::IrcCodec;
use irc::proto::error::ProtocolError;
use futures::{StreamExt, SinkExt};
use futures::stream::SplitStream;
use futures::future::FutureExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use tokio::task::unconstrained;

// matrix stuff
use matrix_sdk::{
    self,
    events::{
        room::message::{MessageEventContent, MessageType, TextMessageEventContent,
            NoticeMessageEventContent, EmoteMessageEventContent, ImageMessageEventContent,
            VideoMessageEventContent, AudioMessageEventContent, FileMessageEventContent,
            LocationMessageEventContent},
        room::member::{MemberEventContent, MembershipState},
        AnyMessageEventContent, AnySyncMessageEvent, AnySyncRoomEvent, SyncMessageEvent,
        AnyToDeviceEvent, AnySyncStateEvent, SyncStateEvent,
    },
    Client, ClientConfig, SyncSettings, Session, RoomMember,
    room, Sas, LoopCtrl,
};
use matrix_sdk_common::{
    MilliSecondsSinceUnixEpoch,
    identifiers::{UserId, RoomId, MxcUri},
    deserialized_responses::SyncResponse,
    api::r0::message::send_message_event,
};
use std::convert::TryFrom;
use std::convert::From;
use std::path::PathBuf;
use url::Url;


#[tokio::main]
async fn main() {
    env_logger::init();
    let my_path = dirs::config_dir().and_then(|a| Some(a.join("matrirc/config"))).unwrap();
    if let Err(e) = dotenv::from_path(my_path.as_path()) {
        debug!("Could not init dotenv file at {:?} : {}", my_path, e);
    }

    while let Err(e) = main_impl().await {
        error!("Error: {:?}", e);
    }
}

async fn main_impl() -> Result<()> {
    // ircd side init
    let ircd = ircd_init().await;
    ircd.await?;
    Ok(())
}

fn sanitize_name<S: AsRef<str>>(s: S) -> String {
    s.as_ref().replace(" ", "_").replace("@", "")
}

struct Chan {
    /// matrix room id
    room_id: RoomId,
    /// joined members (includes yourself!), matrix UserId to nick in chan
    members: HashMap<UserId, ()>,
}

#[derive(Clone)]
enum Query {
    /// matrix room id
    Room(RoomId),
}

#[derive(Clone)]
struct Irc {
    /// irc nick
    nick: Arc<String>,
    /// sink where to send irc messages to
    sink: Arc<Mutex<mpsc::Sender<Message>>>,
    /// stop irc read thread when receive here
    stop_receive: Arc<Mutex<mpsc::Sender<()>>>,
    /// index of channels and who is in
    chans: Arc<RwLock<HashMap<String, Chan>>>,
    /// index of queries
    queries: Arc<RwLock<HashMap<String, Query>>>,
    /// map of matrix user ids to nick
    user_id2nick: Arc<RwLock<HashMap<UserId, String>>>,
    /// map of irc nicks to matrix user ids
    nick2user_id: Arc<RwLock<HashMap<String, UserId>>>,
}

struct MatrixThread {
    /// stop matrix threads when we notice
    stop: bool,
    /// indicate if matrix sync thread is running
    running: bool,
}

#[derive(Clone)]
struct Matrirc {
    irc: Irc,
    /// bool to flag the backlog :/
    initial_sync: Arc<RwLock<bool>>,
    /// matrix sdk client
    matrix_client: Client,
    /// matrix thread state
    matrix_thread: Arc<Mutex<MatrixThread>>,
    /// matrix RoomId to IRC channels
    roomid2chan: Arc<RwLock<HashMap<RoomId, String>>>,
}

impl Matrirc {
    pub fn new(irc_nick: String,
               sink: mpsc::Sender<Message>,
               stop_irc: mpsc::Sender<()>,
               matrix_client: Client,
               ) -> Matrirc {
        Matrirc {
            irc: Irc {
                nick: Arc::new(irc_nick),
                sink: Arc::new(Mutex::new(sink)),
                stop_receive: Arc::new(Mutex::new(stop_irc)),
                chans: Arc::new(RwLock::new(HashMap::<String, Chan>::new())),
                queries: Arc::new(RwLock::new(HashMap::<String, Query>::new())),
                user_id2nick: Arc::new(RwLock::new(HashMap::<UserId, String>::new())),
                nick2user_id: Arc::new(RwLock::new(HashMap::<String, UserId>::new())),
            },
            matrix_client,
            initial_sync: Arc::new(RwLock::new(true)),
            matrix_thread: Arc::new(Mutex::new(MatrixThread { stop: false, running: false })),
            roomid2chan: Arc::new(RwLock::new(HashMap::<RoomId, String>::new())),
        }
    }

    pub async fn sync_forever(&self, mut stream: SplitStream<Framed<TcpStream, IrcCodec>>, mut stopirc: mpsc::Receiver<()>) {
        while let Some(event) = stream.next().await {
            // https://github.com/tokio-rs/tokio/issues/3350
            // tl;dr tokio mpsc try_recv was removed, hack until it's back..
            if let Some(_) = unconstrained(stopirc.recv()).now_or_never() {
                info!("Stopping irc receive thread from other thread request")
            }
            match self.handle_irc_event(event).await {
                Ok(true) => (),
                Ok(false) => {
                    info!("Stopping irc receive thread from callback");
                    break;
                }
                Err(e) => {
                    debug!("Got an error handling irc event: {:?}", e);
                }
            }
        }
    }
    pub async fn irc_stop_receive(&self) {
        let _ = self.irc.stop_receive.lock().await.send(());
    }

    pub async fn reset(&self, toirc: mpsc::Sender<Message>) {
        *self.irc.sink.lock().await = toirc;
        self.irc.chans.write().await.clear();
        self.irc.queries.write().await.clear();
        self.irc.user_id2nick.write().await.clear();
        self.irc.nick2user_id.write().await.clear();
        self.roomid2chan.write().await.clear();
    }

    async fn handle_irc_event(&self, event: Result<Message, ProtocolError>) -> Result<bool> {
        let event = event.context("protocol error")?;
        match &event.command {
            Command::PING(e, o) => {
                debug!("PING {}", e);
                self.irc_send_cmd(None, Command::PONG(e.to_owned(), o.to_owned())).await?;
            }
            Command::QUIT(_) => {
                return Ok(false);
            }
            // XXX after joining a chan, we get a `MODE #chan` query but that currently
            // parses as invalid Message.
            // Normal chan joining exchange:
            // -> :usermask JOIN #chan
            // <- MODE #chan
            // -> :matrirc 324 nick #chan +
            // -> :matrirc 329 nick #chan 1601930643 // chan creation timestamp, optional
            // <- WHO #chan
            // -> :matrirc 352 nick #chan someusername someip someserver somenick H :0 Real Name
            // (repeat for each user -- apparently ok to just skip to the end.)
            // (H = here or G = gone (away), * = oper, @ = chanop, + = voice)
            // -> :matrirc 315 nick #chan :End
            // <- MODE #bar b
            // -> :matrirc 368 nick #chan :End
            Command::PRIVMSG(target, body) => {
                match self.irc_target2query(target).await {
                    Some(Query::Room(room_id)) => {
                        if let Some(action) = body.strip_prefix("\x01ACTION ").and_then(|s| { s.strip_suffix("\x01") }) {
                            if let Err(e) = self.matrix_room_send_emote(&room_id, action.into()).await {
                                self.irc_send_notice("matrirc", &self.irc.nick, &format!("Failed sending to matrix: {:?}", e)).await?;
                            }
                        } else {
                            if let Err(e) = self.matrix_room_send_text(&room_id, body.to_owned()).await {
                                self.irc_send_notice("matrirc", &self.irc.nick, &format!("Failed sending to matrix: {:?}", e)).await?;
                            }
                        }
                    }
                    None => {
                        self.irc_send_notice("matrirc", &self.irc.nick, &format!("Could not find where to send to? {}", target)).await?;
                    }
                }
            }
            Command::NOTICE(target, body) => {
                match self.irc_target2query(target).await {
                    Some(Query::Room(room_id)) => {
                        if let Err(e) = self.matrix_room_send_notice(&room_id, body.to_owned()).await {
                            self.irc_send_notice("matrirc", &self.irc.nick, &format!("Failed sending to matrix: {:?}", e)).await?;
                        }
                    }
                    None => {
                        self.irc_send_notice("matrirc", &self.irc.nick, &format!("Could not find where to send to? {}", target)).await?;
                    }
                }
            }
            _ => debug!("got msg {:?}", event),
        };
        Ok(true)
    }

    /// build list of chan members and join
    pub async fn irc_join_chan(&self, room: room::Joined) -> Result<()> {
        let room_id = room.room_id();
        if self.roomid2chan.read().await.get(&room_id) != None {
            // already done
            return Ok(());
        }
        // direct chats handled as query, only setup maps
        if let Some(user_id) = &room.direct_target() {
            let member = room.get_member(user_id).await?
                .context("room direct target but not in room?")?; // XXX can happen
            let nick = self.matrix_userid2irc_nick_or_set(user_id, &member).await;
            self.roomid2chan.write().await.insert(room_id.clone(), nick.clone());
            self.irc.queries.write().await.insert(nick, Query::Room(room_id.clone()));
            return Ok(());
        }


        // find a suitable chan name
        let mut chan = format!("#{}", sanitize_name(room.display_name().await?));
        while let Some(_) = self.irc.chans.read().await.get(&chan) {
            chan.push('_');
        }
        self.irc_send_cmd(Some(&self.irc.nick), Command::JOIN(chan.clone(), None, None)).await?;

        // build member list
        let mut chan_members = HashMap::<UserId, ()>::new();
        let names_list_header = format!(":matrirc 353 {} = {} :", self.irc.nick, chan);
        let mut names_list = names_list_header.clone();
        // insert self first to reserve nick
        names_list.push_str(&self.irc.nick);
        names_list.push(' ');
        let self_user_id = self.matrix_client.user_id().await.unwrap();
        chan_members.insert(self_user_id.clone(), ());
        for member in room.joined_members().await?.iter() {
            if member.user_id().as_ref() == self_user_id {
                continue;
            }
            let nick = self.matrix_userid2irc_nick_or_set(member.user_id(), member).await;
            names_list.push_str(&nick);
            names_list.push(' ');
            if names_list.len() > 400 {
                self.irc_send_raw(&names_list).await?;
                names_list = names_list_header.clone();
            }
            chan_members.insert(member.user_id().clone(), ());
        }
        if names_list != names_list_header {
                self.irc_send_raw(&names_list).await?;
        }
        self.irc_send_raw(&format!(":matrirc 366 {} {} :End", self.irc.nick, chan)).await?;
        self.roomid2chan.write().await.insert(room_id.clone(), chan.clone());
        self.irc.chans.write().await.insert(chan, Chan {
            room_id: room_id.clone(),
            members: chan_members,
        });
        Ok(())
    }

    async fn matrix_userid2irc_nick_or_set(&self, user_id: &UserId, member: &RoomMember) -> String {
        match self.matrix_userid2irc_nick(user_id).await {
            Some(nick) => nick, // XXX potentially update nick here
            None => {
                let mut nick: String = match &member.display_name() {
                    Some(name) => name.to_string(),
                    None => {
                        debug!("member has no name: {:?}", member);
                        member.name().to_string()
                    },
                };
                nick = sanitize_name(nick);
                while self.irc_nick2matrix_userid(&nick).await.is_some() {
                    nick.push('_');
                }
                self.insert_userid_nick_maps(user_id, nick).await
            }
        }
    }
    /// helper to get http url instead of mxc
    pub fn mxcuri_option_to_string(&self, uri: Option<MxcUri>) -> String {
        let url = match uri {
            Some(uri) => uri.as_str().to_string(),
            None => "no_url".to_string(),
        };
        url.replace("mxc://", &format!("{}/_matrix/media/r0/download/", self.matrix_client.homeserver()))
            .replace("//", "/")
    }


    /// helper to send custom Message
    pub async fn irc_send_cmd(&self, prefix: Option<&str>, command: Command) -> Result<()> {
        let sink = self.irc.sink.lock().await.clone();
        sink.send(Message {
            tags: None,
            prefix: prefix.and_then(|p| { Some(Prefix::new_from_str(p)) }),
            command,
        }).await.context("could not send to pipe to irc messages")
    }
    /// helper to send raw tex
    pub async fn irc_send_raw(&self, message: &str) -> Result<()> {
        self.irc_send_cmd(None, Command::Raw(message.into(), vec![])).await
    }
    /// helper for privmsg
    pub async fn irc_send_privmsg(&self, prefix: &str, target: &str, message: &str) -> Result<()> {
        self.irc_send_cmd(Some(prefix), Command::PRIVMSG(target.into(), message.into())).await
    }
    /// helper for notice
    pub async fn irc_send_notice(&self, prefix: &str, target: &str, message: &str) -> Result<()> {
        self.irc_send_cmd(Some(&format!("{}!m", prefix)), Command::NOTICE(target.into(), message.into())).await
    }

    pub async fn matrix_room_send_text(&self, room_id: &RoomId, body: String) -> Result<send_message_event::Response> {
        let response = AnyMessageEventContent::RoomMessage(MessageEventContent::text_plain(body));
        Ok(self.matrix_client.room_send(&room_id, response, None).await?)
    }

    pub async fn matrix_room_send_notice(&self, room_id: &RoomId, body: String) -> Result<send_message_event::Response> {
        let response = AnyMessageEventContent::RoomMessage(MessageEventContent::notice_plain(body));
        Ok(self.matrix_client.room_send(&room_id, response, None).await?)
    }

    pub async fn matrix_room_send_emote(&self, room_id: &RoomId, body: String) -> Result<send_message_event::Response> {
        let response = AnyMessageEventContent::RoomMessage(MessageEventContent::new(
            MessageType::Emote(EmoteMessageEventContent::plain(body))));
        Ok(self.matrix_client.room_send(&room_id, response, None).await?)
    }

    pub async fn handle_matrix_events(&self, response: SyncResponse) -> Result<()> {
        for (room_id, room) in response.rooms.join {
            for event in &room.state.events {
                if let Err(e) = self.handle_matrix_room_state_event_raw(&room_id, event).await {
                    warn!("room state event error: {:?}", e);
                }
            }
            for event in room.timeline.events {
                if let Err(e) = self.handle_matrix_room_timeline_event_raw(&room_id, &event).await {
                    warn!("Event {:?} errored: {:?}", event, e);
                }
            }
        }

        for event in response.to_device.events {
            if let Err(e) = self.handle_matrix_device_event_raw(&event).await {
                warn!("Event {:?} errored: {:?}", event, e);
            }
        }

        Ok(())
    }

    async fn handle_matrix_room_timeline_event_raw(&self, room_id: &RoomId, raw_event: &matrix_sdk::deserialized_responses::SyncRoomEvent) -> Result<(), Error> {
        let event = raw_event.event.deserialize()?;
        trace!("room timeline event {:?}", event);
        match event {
            AnySyncRoomEvent::Message(AnySyncMessageEvent::RoomMessage(ev)) => {
                let SyncMessageEvent { content, sender, origin_server_ts: ts, unsigned, .. } = ev;
                if unsigned.transaction_id != None && ! *self.initial_sync.read().await {
                    // this apparently means it's our echo message, skip it
                    return Ok(());
                }
                let (chan, sender, real_sender) = self.matrix2irc_targets(&room_id, &sender).await
                    .unwrap_or((self.irc.nick.to_string(), "matrirc".to_string(), "<Error getting channel>".to_string()));
                let msg_prefix = if sender == real_sender {
                    "".to_string()
                } else {
                    format!("<from {}> ", real_sender)
                };
                let MessageEventContent { msgtype, relates_to, new_content, .. } = content;
                match msgtype {
                    MessageType::Text(TextMessageEventContent { body: msg_body, .. }) =>  {
                        let msg_body = self.body_prepend_ts(msg_body, ts).await;
                        for line in msg_body.split('\n') {
                            self.irc_send_privmsg(&sender, &chan, &format!("{}{}", msg_prefix, line)).await?;
                        }
                    }
                    MessageType::Notice(NoticeMessageEventContent { body: msg_body, .. }) => {
                        let msg_body = self.body_prepend_ts(msg_body, ts).await;
                        for line in msg_body.split('\n') {
                            self.irc_send_notice(&sender, &chan, &format!("{}{}", msg_prefix, line)).await?;
                        }
                    }
                    MessageType::Emote(EmoteMessageEventContent { body: msg_body, .. }) => {
                        let msg_body = self.body_prepend_ts(msg_body, ts).await;
                        for line in msg_body.split('\n') {
                            self.irc_send_privmsg(&sender, &chan, &format!("\x01ACTION {}{}\x01", msg_prefix, line)).await?;
                        }
                    }
                    MessageType::Audio(AudioMessageEventContent{ body, url, .. }) => {
                        let url = self.mxcuri_option_to_string(url);

                        let msg_body = format!("<{} sent audio: {} @ {}>", real_sender, body, url);
                        let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                        self.irc_send_notice(&sender, &chan, &msg_body).await?;
                    }
                    MessageType::File(FileMessageEventContent{ body, url, .. }) => {
                        let url = self.mxcuri_option_to_string(url);

                        let msg_body = format!("<{} sent a file: {} @ {}>", real_sender, body, url);
                        let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                        self.irc_send_notice(&sender, &chan, &msg_body).await?;
                    }
                    MessageType::Image(ImageMessageEventContent{ body, url, .. }) => {
                        let url = self.mxcuri_option_to_string(url);

                        let msg_body = format!("<{} sent an image: {} @ {}>",
                                               real_sender, body, url);
                        let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                        self.irc_send_notice(&sender, &chan, &msg_body).await?;
                    }
                    MessageType::Location(LocationMessageEventContent{ body, geo_uri, .. }) => {
                        let msg_body = format!("<{} sent a location: {} @ {}>", real_sender, body, geo_uri);
                        let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                        self.irc_send_notice(&sender, &chan, &msg_body).await?;
                    }
                    MessageType::Video(VideoMessageEventContent{ body, url, .. }) => {
                        let url = self.mxcuri_option_to_string(url);

                        let msg_body = format!("<{} sent a video: {} @ {}>", real_sender, body, url);
                        let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                        self.irc_send_notice(&sender, &chan, &msg_body).await?;
                    }
                    _ => {
                        debug!("other content? {:?}", msgtype)
                    }
                }
            }
            AnySyncRoomEvent::Message(AnySyncMessageEvent::RoomEncrypted(ev)) => {
                let SyncMessageEvent { sender, origin_server_ts: ts, .. } = ev;
                let chan = self.matrix_room2irc_chan(&room_id).await;
                let sender = self.matrix_userid2irc_nick_default(&sender).await;
                let msg_body = "Could not decrypt message";
                let msg_body = self.body_prepend_ts(msg_body.into(), ts).await;
                self.irc_send_privmsg(&sender, &chan, &msg_body).await?;
            }
            AnySyncRoomEvent::State(ev) => {
                self.handle_matrix_room_state_event(room_id, ev).await?;
            }
            _  => {
                    debug!("unhandled room timeline event {:?}", event);
            }
        }
        Ok(())
    }

    async fn handle_matrix_room_state_event_raw(&self, room_id: &RoomId, raw_event: &matrix_sdk::Raw<AnySyncStateEvent>) -> Result<(), Error> {
        let event = raw_event.deserialize()?;
        trace!("room state event {:?}", event);
        self.handle_matrix_room_state_event(room_id, event).await
    }

    async fn handle_matrix_room_state_event(&self, room_id: &RoomId, event: AnySyncStateEvent) -> Result<(), Error> {
        match event {
            AnySyncStateEvent::RoomCreate(_) => {
                for room in &self.matrix_client.joined_rooms() {
                    if room.room_id() == room_id {
                        return self.irc_join_chan(room.clone()).await;
                    }
                }
            }
            AnySyncStateEvent::RoomMember(ev) => {
                let SyncStateEvent{
                    content: MemberEventContent{ displayname, membership, .. },
                    sender, .. } = ev;
                match membership {
                    MembershipState::Leave => {
                        let chan = self.matrix_room2irc_chan(room_id).await;
                        let nick = self.matrix_userid2irc_nick_default(&sender).await;
                        self.irc_send_cmd(Some(&nick), Command::PART(chan, None)).await?;
                    },
                    MembershipState::Join => {
                        let display_name = match displayname {
                            Some(name) => name,
                            None => sender.to_string(),
                        };
                        let chan = self.matrix_room2irc_chan(room_id).await;
                        self.irc_member_join(&chan, &sender, &display_name).await?;
                    }
                    _ => {
                        debug!("unknown membership {} for {} in {}", membership, sender, room_id);
                    }
                }
            }
            _ => {
                    debug!("unhandled room state event {:?}", event);
            }
        }
        Ok(())
    }

    async fn handle_matrix_device_event_raw(&self, raw_event: &matrix_sdk::Raw<AnyToDeviceEvent>) -> Result<(), Error> {
        let event = raw_event.deserialize()?;
        trace!("device event {:?}", event);
        match event {
            AnyToDeviceEvent::KeyVerificationStart(e) => {
                let sas = self.matrix_client.get_verification(&e.content.transaction_id).await
                    .context("Get verification for start event")?;

                info!("Starting verification with {} {}",
                      &sas.other_device().user_id(),
                      &sas.other_device().device_id()
                );
                sas.accept().await?;
            }
            AnyToDeviceEvent::KeyVerificationKey(e) => {
                let sas = self.matrix_client.get_verification(&e.content.transaction_id).await
                    .context("Get verification for key event")?;

                tokio::spawn(self.clone().confirm_key_verification(sas));
            }
            AnyToDeviceEvent::KeyVerificationMac(e) => {
                let sas = self.matrix_client.get_verification(&e.content.transaction_id).await
                    .context("Get verification for mac event")?;

                if sas.is_done() {
                    let device = sas.other_device();

                    info!("Successfully verified device {} {}",
                          device.user_id(), device.device_id());
                } else {
                    info!("Key Verification Mac failed?");
                }

            }
            // [2020-10-05T12:50:18Z DEBUG matrirc] unhandled event: RoomKeyRequest(ToDeviceEvent { content: RoomKeyRequestEventContent { action: Request, body: Some(RequestedKeyInfo { algorithm: MegolmV1AesSha2, room_id: RoomId { full_id: "!roomid", colon_idx: 19 }, sender_key: "xxx", session_id: "xxx" }), requesting_device_id: DeviceId("xxx"), request_id: "xxx" }, sender: UserId { full_id: "xxx", colon_idx: 9, is_historical: false } })
            // XXX handle when todo gone from sdk?
            e => {
                debug!("unhandled event: {:?}", e);
            }
        }
        Ok(())
    }

    async fn body_prepend_ts(&self, msg_body: String, ts: MilliSecondsSinceUnixEpoch) -> String {
        if *self.initial_sync.read().await {
            let datetime: DateTime<Local> = ts.to_system_time().unwrap_or(SystemTime::UNIX_EPOCH).into();
            return format!("{} {}", datetime.format("<%Y-%m-%d %H:%M:%S>"), msg_body);
        }
        msg_body
    }

    async fn confirm_key_verification(self, sas: Sas) -> Result<()> {
        println!("Do the emoji match? {:?}", sas.emoji());

        if dialoguer::Confirm::new().with_prompt("Accept verification?").interact()? {
            println!("Confirming verification...");
            sas.confirm().await?;

            if sas.is_done() {
                let device = sas.other_device();

                info!("Successfully verified device {} {}",
                      device.user_id(), device.device_id());
            }

            Ok(())
        } else {
            println!("Aborting verification");

            sas.cancel().await.context("could not disable verification")
        }
    }

    async fn matrix2irc_targets(&self, room_id: &RoomId, sender: &UserId) -> Option<(String, String, String)> {
        let chan = self.matrix_room2irc_chan(&room_id).await;
        let sender = self.matrix_userid2irc_nick_default(&sender).await;
        if chan.chars().next()? == '#' {
            Some((chan, sender.clone(), sender))
        } else {
            Some((self.irc.nick.to_string(), chan, sender))
        }
    }

    async fn irc_target2query(&self, target: &str) -> Option<Query> {
        if target.chars().next()? == '#' {
            let map_locked = self.irc.chans.read().await;
            let chan = map_locked.get(target)?;
            Some(Query::Room(chan.room_id.clone()))
        } else {
            Some(self.irc.queries.read().await.get(target)?.clone())
        }
    }

    async fn matrix_room2irc_chan(&self, room_id: &RoomId) -> String {
        let map_locked = self.roomid2chan.read().await;
        match map_locked.get(room_id) {
            Some(chan) => chan.clone(),
            None => "&matrirc".to_string(),
        }
    }

    /// insert new nick, preferring old one if found
    async fn insert_userid_nick_maps(&self, user_id: &UserId, nick: String) -> String {
        let nick = self.irc.user_id2nick.write().await
            .entry(user_id.clone())
            .or_insert(nick)
            .clone();
        self.irc.nick2user_id.write().await
            .insert(nick.clone(), user_id.clone());
        nick
    }

    /// insert anyway
    /// XXX racy because two locks, but we can only be updated
    /// from the matrix event handler thread... ideally merge locks later
    async fn update_userid_nick_maps(&self, user_id: &UserId, nick: String) {
        let mut query = None;
        if let Some(old_nick) = self.irc.user_id2nick.read().await.get(user_id).clone() {
            self.irc.nick2user_id.write().await
                .remove(old_nick);
            query = self.irc.queries.write().await.remove(old_nick);
        }
        self.irc.user_id2nick.write().await
            .insert(user_id.clone(), nick.clone());
        self.irc.nick2user_id.write().await
            .insert(nick.clone(), user_id.clone());
        if let Some(query) = query {
            match &query {
                Query::Room(room_id) => {
                    self.roomid2chan.write().await.insert(room_id.clone(), nick.clone());
                }
            }
            self.irc.queries.write().await.insert(nick.clone(), query);
        }

    }

    async fn matrix_userid2irc_nick(&self, user_id: &UserId) -> Option<String> {
        Some(self.irc.user_id2nick.read().await.get(user_id)?.clone())
    }

    async fn matrix_userid2irc_nick_default(&self, user_id: &UserId) -> String {
        self.irc.user_id2nick.read().await.get(user_id)
             .unwrap_or(&user_id.to_string()).clone()
    }

    async fn irc_nick2matrix_userid(&self, nick: &str) -> Option<UserId> {
        Some(self.irc.nick2user_id.read().await.get(nick)?.clone())
    }

    async fn irc_member_join(&self, chan_name: &str, user_id: &UserId, nick: &str) -> Result<()> {
        let nick = sanitize_name(nick);
        match self.matrix_userid2irc_nick(user_id).await {
            None => {
                self.update_userid_nick_maps(user_id, nick.clone()).await;
            }
            Some(old_nick) => {
                debug!("Considering rename {} to {}", old_nick, nick);
                if old_nick.as_str() != nick &&
                    self.irc_nick2matrix_userid(&nick).await.is_none() {
                    self.update_userid_nick_maps(user_id, nick.clone()).await;
                    self.irc_send_cmd(Some(&old_nick), Command::NICK(nick.clone())).await?;
                }
            }
        }

        let join = {
            let mut map_locked = self.irc.chans.write().await;
            let chan = map_locked.entry(chan_name.into());
            let mut chan = match chan {
                Entry::Vacant(_) => return Err(Error::msg("someone joining a chan we don't know?")),
                Entry::Occupied(chan) => chan,
            };
            let chan = chan.get_mut();

            match chan.members.entry(user_id.clone()) {
                Entry::Occupied(_) => {
                    false
                }
                Entry::Vacant(e) => {
                    e.insert(());
                    true
                }
            }
        };
        if join {
            self.irc_send_cmd(Some(&nick), Command::JOIN(chan_name.into(), None, None)).await?;
        }
        Ok(())
    }
}

async fn matrix_init(pickle_pass: Option<String>) -> matrix_sdk::Client {
    let homeserver = dotenv::var("HOMESERVER").context("HOMESERVER is not set").unwrap();
    let store_path = match dotenv::var("STORE_PATH") {
        Ok(path) => PathBuf::from(&path),
         _ => dirs::data_dir().and_then(|a| Some(a.join("matrirc/matrix_store"))).unwrap()
    };

    let client_config = ClientConfig::new().store_path(store_path);
    let client_config = match pickle_pass {
        Some(pass) => client_config.passphrase(pass),
        None => client_config,
    };
    let homeserver_url = Url::parse(&homeserver).expect("Couldn't parse the homeserver URL");
    let client = Client::new_with_config(homeserver_url, client_config).unwrap();

    if let Ok(access_token) = dotenv::var("ACCESS_TOKEN") {
        let user_id = dotenv::var("USER_ID").context("USER_ID not defined when ACCESS_TOKEN is").unwrap();
        let device_id = dotenv::var("DEVICE_ID").context("DEVICE_ID not found when ACCESS_TOKEN is").unwrap();
        let session = Session {
            access_token,
            user_id: UserId::try_from(user_id).unwrap(),
            device_id: device_id.into(),
        };

        client.restore_login(session).await.context("Restore login failed").unwrap();
    } else {
        info!("No access token: prompting for user/pass");
        let username = dialoguer::Input::<String>::new().with_prompt("Matrix username").interact().unwrap();
        let password = dialoguer::Password::new().with_prompt("Matrix password").interact().unwrap();

        let login = client.login(&username, &password, None, Some("matrirc")).await.context("Login failed").unwrap();

        println!("To keep the current session, define the following:");
        println!("ACCESS_TOKEN={}", login.access_token);
        println!("USER_ID={}", login.user_id);
        println!("DEVICE_ID={}", login.device_id);
    }

    client
}

async fn ircd_init() -> tokio::task::JoinHandle<()> {
    let bind_address = dotenv::var("IRCD_BIND_ADDRESS").unwrap_or("[::1]:6667".to_string());
    info!("listening on {}", bind_address);
    let listener = TcpListener::bind(bind_address).await.context("bind ircd port").unwrap();
    tokio::spawn(async move {
        let matrirc = Arc::new(Mutex::new(None));
        while let Ok((socket, addr)) = listener.accept().await {
          info!("accepted connection from {}", addr);
          let codec = IrcCodec::new("utf-8").unwrap();
          let stream = Framed::new(socket, codec);
          let matrirc_clone = matrirc.clone();
          tokio::spawn(async move {
            handle_irc(matrirc_clone, stream).await
          });
        }
        info!("listener died");
    })
}

async fn irc_auth_loop(mut stream: Framed<TcpStream, IrcCodec>) -> Result<(String, Option<String>, Framed<TcpStream, IrcCodec>)> {
    let mut authentified = if let Err(_) = dotenv::var("IRC_PASSWORD") { true } else { false };
    let mut irc_nick = None;
    let mut pickle_pass = None;
    while let Some(Ok(event)) = stream.next().await {
        match event.command {
            Command::PASS(user_pass) => {
                if let Ok(config_pass) = dotenv::var("IRC_PASSWORD") {
                    debug!("Checking password");
                    let split: Vec<_> = user_pass.splitn(2, ':').collect();
                    if split.len() == 2 {
                        pickle_pass = Some(split[1].to_string());
                    }
                    let user_pass = split[0].to_string();

                    if user_pass == config_pass {
                        // XXX define some separator and set second half as picke passphrase
                        authentified = true;
                    } else {
                        stream.send(Message {
                            command: Command::Raw(format!("Bad Password given"), vec![]),
                            tags: None,
                            prefix: Some(Prefix::new_from_str("matrircd")),
                        }).await.context("refuse bad passowrd").unwrap();
                        warn!("Bad login attempt");
                        stream.flush().await?;
                        return Err(Error::msg("Bad password"));
                    }
                }
            }
            Command::NICK(nick) => {
                irc_nick = Some(nick)
            }
            Command::USER(user, _, _) => {
                if ! authentified {
                    stream.send(Message {
                        command: Command::Raw(format!("Authenticate yourself!"), vec![]),
                        tags: None,
                        prefix: Some(Prefix::new_from_str("matrircd")),
                    }).await.context("refuse unauthenticated login").unwrap();
                    warn!("Attempt to login without password");
                    stream.flush().await?;
                    return Err(Error::msg("Unauthenticated login"));
                }
                let nick = match irc_nick {
                    Some(nick) => nick,
                    None => user,
                };
                return Ok((nick, pickle_pass, stream));
            }
            _ => trace!("got preauth message {:?}", event),
        }
    }
    warn!("Stream ended before USER command");
    return Err(Error::msg("Stream ended before USER command"))
}

fn irc_raw_msg(msg: String) -> Message {
    Message {
        tags: None,
        prefix: None,
        command: Command::Raw( msg, vec![]),
    }
}

async fn handle_irc(
    global_matrirc: Arc<Mutex<Option<Matrirc>>>,
    stream: Framed<TcpStream, IrcCodec>,
) -> Result<()> {
    // Check auth before doing anything else
    let (irc_nick, pickle_pass, stream) = irc_auth_loop(stream).await?;
    let (mut writer, reader) = stream.split();
    writer.send(irc_raw_msg(format!(":{} 001 {} :Welcome to matrirc", "matrirc", irc_nick))).await?;
    let (toirc, toirc_rx) = mpsc::channel::<Message>(100);
    let (stopirc, stopirc_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move { irc_write_thread(writer, toirc_rx).await });

    let (matrirc, initial_response) = {
        let mut global_lock = global_matrirc.lock().await;
        match &*global_lock {
            Some(matrirc) => {
                // just in case... try to quit previous IRC thread
                let _ = matrirc.irc_send_cmd(None, Command::QUIT(None)).await;
                let _ = matrirc.irc_stop_receive().await;
                matrirc.reset(toirc).await;
                (matrirc.to_owned(), None)
            }
            None => {
                let matrix_client = matrix_init(pickle_pass).await;

                // initial matrix sync required for room list
                let matrix_response = matrix_client.sync_once(SyncSettings::new()).await?;
                trace!("initial sync: {:#?}", matrix_response);

                let matrirc = Matrirc::new(irc_nick.clone(), toirc, stopirc, matrix_client.clone());
                *global_lock = Some(matrirc.clone());
                (matrirc, Some(matrix_response))
            }
        }
    };
    let matrix_client = matrirc.matrix_client.clone();

    // XXX move that in new somehow but await..
    let self_user_id = &matrirc.matrix_client.user_id().await.unwrap();
    matrirc.update_userid_nick_maps(self_user_id, matrirc.irc.nick.to_string()).await;

    for room in matrix_client.joined_rooms() {
        matrirc.irc_join_chan(room.clone()).await?;
    }

    let spawn_matrix = {
        let mut thread_lock = matrirc.matrix_thread.lock().await;
        if thread_lock.running {
            thread_lock.stop = false;
            false
        } else {
            true
        }
    };

    if spawn_matrix {
        if let Some(matrix_response) = initial_response {
            matrirc.handle_matrix_events(matrix_response).await?;
        }
        *matrirc.initial_sync.write().await = false;

        matrirc.matrix_thread.lock().await.running = true;
        let matrirc_clone = matrirc.clone();
        tokio::spawn(async move {
            matrix_client.sync_with_callback(SyncSettings::new(), |resp| async {
                trace!("got {:#?}", resp);
                matrirc_clone.handle_matrix_events(resp).await.unwrap();
                let mut thread_lock = matrirc_clone.matrix_thread.lock().await;
                if thread_lock.stop {
                    thread_lock.stop = false;
                    thread_lock.running = false;
                    LoopCtrl::Break
                } else {
                    LoopCtrl::Continue
                }
            }).await
        });
    }
    matrirc.sync_forever(reader, stopirc_rx).await;

    info!("disconnect event");
    matrirc.matrix_thread.lock().await.stop = true;
    matrirc.irc_send_cmd(None, Command::QUIT(None)).await?;
    Ok(())
}

async fn irc_write_thread(mut writer: futures::stream::SplitSink<Framed<TcpStream, IrcCodec>, Message>, mut toirc_rx: mpsc::Receiver<Message>) -> Result<()> {
    while let Some(message) = toirc_rx.recv().await {
        match message.command {
            Command::QUIT(_) => {
                writer.close().await?;
                info!("Stopping write thread to quit");
                return Ok(());
            }
            _ => {
                writer.send(message).await?;
            }
        }
    }
    info!("Stopping write thread to some error");
    Ok(())
}
