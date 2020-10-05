//! spilo - a minimalistic IRC bouncer
//! Copyright (C) 2018 Aaron Weiss
//!
//! This program is free software: you can redistribute it and/or modify
//! it under the terms of the GNU Affero General Public License as published by
//! the Free Software Foundation, either version 3 of the License, or
//! (at your option) any later version.
//!
//! This program is distributed in the hope that it will be useful,
//! but WITHOUT ANY WARRANTY; without even the implied warranty of
//! MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
//! GNU Affero General Public License for more details.
//!
//! You should have received a copy of the GNU Affero General Public License
//! along with this program.  If not, see <https://www.gnu.org/licenses/>.

extern crate env_logger;
extern crate anyhow;
#[macro_use]
extern crate log;
extern crate irc;
extern crate tokio;
extern crate tokio_util;
extern crate dialoguer;

use anyhow::{Result, Context, Error};
use futures::prelude::*;
use tokio::sync::mpsc;
use futures::stream::{SplitSink, SplitStream};
use std::collections::HashMap;
use std::sync::{Arc, RwLock, Mutex};

// ircd stuff
use irc::client::prelude::*;
use irc::proto::IrcCodec;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Decoder, Framed};

// matrix stuff
use matrix_sdk::{
    self,
    events::{
        room::message::{MessageEventContent, TextMessageEventContent},
        AnyMessageEventContent, AnySyncMessageEvent, AnySyncRoomEvent, SyncMessageEvent,
    },
    Client, ClientConfig, EventEmitter, SyncRoom, SyncSettings, Session, Room,
};
use matrix_sdk_common::{
    identifiers::{UserId, RoomId},
    api::r0::sync::sync_events::Response,
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
    s.as_ref().replace(" ", "_")
}

struct Chan {
    /// matrix room id
    room_id: RoomId,
    /// joined members (includes yourself!), matrix UserId to nick in chan
    members2nick: HashMap<UserId, String>,
    /// nick to matrix user id
    /// XXX irc nick to UserId could be implemented to retranscript hilights
    /// to proper matrix hilight later
    nicks2id: HashMap<String, UserId>,
}

#[derive(Clone)]
struct Irc {
    /// irc nick
    nick: Arc<String>,
    /// nick!user@host format
    mask: Arc<String>,
    /// sink where to send irc messages to
    sink: Arc<Mutex<mpsc::Sender<Message>>>,
    /// index of channels and who is in
    chans: Arc<RwLock<HashMap<String, Chan>>>,
}

#[derive(Clone)]
struct Matrirc {
    irc: Irc,
    /// matrix sdk client
    matrix_client: Client,
    /// stop other threads when we notice
    stop: Arc<RwLock<bool>>,
    /// matrix RoomId to IRC channels
    roomid2chan: Arc<RwLock<HashMap<RoomId, String>>>,
}

impl Matrirc {
    pub fn new(irc_nick: String,
               irc_mask: String,
               sink: mpsc::Sender<Message>,
               matrix_client: Client,
               ) -> Matrirc {
        Matrirc {
            irc: Irc {
                nick: Arc::new(irc_nick),
                mask: Arc::new(irc_mask),
                sink: Arc::new(Mutex::new(sink)),
                chans: Arc::new(RwLock::new(HashMap::<String, Chan>::new())),
            },
            matrix_client,
            stop: Arc::new(RwLock::new(false)),
            roomid2chan: Arc::new(RwLock::new(HashMap::<RoomId, String>::new())),
        }
    }

    pub async fn sync_forever(&self, mut stream: SplitStream<Framed<TcpStream, IrcCodec>>) -> Result<()> {
        while let Some(event) = stream.next().await {
            let event = match event {
                Err(e) => {
                    info!("got error {:?}", e);
                    // XXX we get errors because of `MODE #chan` because :
                    // InvalidModeString { string: "", cause: MissingModeModifier } }
                    continue;
                }
                Ok(event) => event,
            };
            match event.command {
                Command::PING(e, o) => {
                    debug!("PING {}", e);
                    self.irc_send_cmd(None, Command::PONG(e, o)).await?;
                }
                Command::QUIT(_) => {
                    debug!("IRC got quit");
                    self.irc_send_cmd(None, Command::QUIT(None)).await?;
                    break;
                }
                _ => info!("got msg {:?}", event),
            }
        }
        debug!("got out of sync_forever irc");
        Ok(())
    }

    /// build list of chan members and join
    pub async fn irc_join(&self, room: Room) -> Result<()> {
        // find a suitable chan name
        let mut chan = format!("#{}", sanitize_name(room.display_name()));
        while let Some(_) = self.irc.chans.read().unwrap().get(&chan) {
            chan.push('_');
        }
        self.irc_send_cmd(Some(&self.irc.mask), Command::JOIN(chan.clone(), None, None)).await?;

        // build member list
        let mut chan_members2nick = HashMap::new();
        let mut chan_nicks2id = HashMap::new();
        let names_list_header = format!(":matrirc 353 {} = {} :", self.irc.nick, chan);
        let mut names_list = names_list_header.clone();
        for (member_id, member) in room.joined_members.iter() {
            let member_name = match &member.display_name {
                Some(name) => name.clone(),
                None => member.name(),
            };
            let mut nick = sanitize_name(member_name);
            while let Some(_) = chan_nicks2id.get(&nick) {
                nick.push('_');
            }
            names_list.push_str(&nick);
            names_list.push(' ');
            if names_list.len() > 400 {
                self.irc_send_raw(&names_list).await?;
                names_list = names_list_header.clone();
            }
            chan_nicks2id.insert(nick.clone(), member_id.clone());
            chan_members2nick.insert(member_id.to_owned(), nick);
        }
        if names_list != names_list_header {
                self.irc_send_raw(&names_list).await?;
        }
        self.irc_send_raw(&format!(":matrirc 366 {} {} :End", self.irc.nick, chan)).await?;
        self.roomid2chan.write().unwrap().insert(room.room_id.clone(), chan.clone());
        self.irc.chans.write().unwrap().insert(chan, Chan {
            room_id: room.room_id,
            members2nick: chan_members2nick,
            nicks2id: chan_nicks2id,
        });
        Ok(())
    }
    /// helper to send custom Message
    pub async fn irc_send_cmd(&self, prefix: Option<&str>, command: Command) -> Result<()> {
        let mut sink = self.irc.sink.lock().unwrap().clone();
        sink.send(Message {
            tags: None,
            prefix: prefix.and_then(|p| { Some(Prefix::new_from_str(p)) }),
            command,
        }).await?;
        Ok(())
    }
    /// helper to send raw tex
    pub async fn irc_send_raw(&self, message: &str) -> Result<()> {
        self.irc_send_cmd(None, Command::Raw(message.into(), vec![])).await
    }
    /// helper for privmsg
    pub async fn irc_send_privmsg(&self, prefix: &str, target: &str, message: &str) -> Result<()> {
        self.irc_send_cmd(Some(prefix), Command::PRIVMSG(target.into(), message.into())).await
    }

    pub async fn handle_matrix_events(&self, response: Response) -> Result<()> {
        for (room_id, room) in response.rooms.join {
            for event in room.timeline.events {
                if let Ok(event) = event.deserialize() {
                    if let AnySyncRoomEvent::Message(AnySyncMessageEvent::RoomMessage(ev)) = event {
                        if let SyncMessageEvent {
                            content: MessageEventContent::Text(TextMessageEventContent { body: msg_body, .. }),
                            sender,
                            origin_server_ts,
                            ..
                        } = ev
                        {
                            let channame = self.roomid2chan.read().unwrap().get(&room_id).unwrap().clone();
                            let nick = {
                                let chans = self.irc.chans.read().unwrap();
                                let chan = chans.get(&channame).unwrap();
                                match chan.members2nick.get(&sender) {
                                    Some(nick) => nick.clone(),
                                    None => sender.to_string(),
                                }
                            };
                            // XXX parse origin_server_ts and prefix message with <timestamp> if
                            // older than X
                            self.irc_send_privmsg(&format!("{}!{}@matrirc", nick, nick), &channame, &msg_body).await?;
                        };
                    }
                }
            }
        }

        Ok(())
    }
}


async fn matrix_init() -> matrix_sdk::Client {
    let homeserver = dotenv::var("HOMESERVER").context("HOMESERVER is not set").unwrap();
    let store_path = match dotenv::var("STORE_PATH") {
        Ok(path) => PathBuf::from(&path),
         _ => dirs::data_dir().and_then(|a| Some(a.join("matrirc/matrix_store"))).unwrap()
    };

    let client_config = ClientConfig::new().store_path(store_path);
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
    let mut listener = TcpListener::bind("[::1]:6667").await.context("bind ircd port").unwrap();
    info!("listening on localhost:6667");
    tokio::spawn(async move {
        while let Ok((socket, addr)) = listener.accept().await {
          info!("accepted connection from {}", addr);
          let codec = IrcCodec::new("utf-8").unwrap();
          let stream = codec.framed(socket);
          tokio::spawn(async move {
            handle_irc(stream).await
          });
        }
        info!("listener died");
    })
}

async fn irc_auth_loop(mut stream: Framed<TcpStream, IrcCodec>) -> Result<(String, String, Framed<TcpStream, IrcCodec>)> {
    let mut authentified = if let Err(_) = dotenv::var("IRC_PASSWORD") { true } else { false };
    let mut irc_nick = None;
    while let Some(Ok(event)) = stream.next().await {
        match event.command {
            Command::PASS(user_pass) => {
                if let Ok(config_pass) = dotenv::var("IRC_PASSWORD") {
                    debug!("Checking password");
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
                    None => user.clone(),
                };
                return Ok((nick, user, stream));
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

async fn handle_irc(stream: Framed<TcpStream, IrcCodec>) -> Result<()> {
    // Check auth before doing anything else
    let (irc_nick, irc_user, stream) = irc_auth_loop(stream).await?;
    let (mut writer, reader) = stream.split();
    writer.send(irc_raw_msg(format!(":{} 001 {} :Welcome to matrirc", "matrirc", irc_nick))).await?;
    let (toirc, toirc_rx) = mpsc::channel::<Message>(100);
    tokio::spawn(async move { irc_write_thread(writer, toirc_rx).await });

    // setup matrix things
    let matrix_client = matrix_init().await;

    // initial matrix sync required for room list
    let matrix_response = matrix_client.sync(SyncSettings::new()).await?;
    trace!("initial sync: {:#?}", matrix_response);

    let matrirc = Matrirc::new(irc_nick.clone(), format!("{}!{}@matrirc", irc_nick, irc_user), toirc, matrix_client.clone());

    for room in matrix_client.joined_rooms().read().await.values() {
        matrirc.irc_join(room.read().await.clone()).await?;
    }

    matrirc.handle_matrix_events(matrix_response).await?;
    let matrirc_clone = matrirc.clone();
    tokio::spawn(async move {
        matrix_client.sync_forever(SyncSettings::new(), |resp| async {
            trace!("got {:#?}", resp);
            matrirc_clone.handle_matrix_events(resp).await.unwrap();
        }).await
    });
    matrirc.sync_forever(reader).await?;

    // XXX cleanup matrix at this point or reconnect won't work
    info!("disconnect event");
    Ok(())
}

async fn irc_write_thread(mut writer: futures::stream::SplitSink<Framed<TcpStream, IrcCodec>, Message>, mut toirc_rx: mpsc::Receiver<Message>) -> Result<()> {
    while let Some(message) = toirc_rx.recv().await {
        match message.command {
            Command::QUIT(_) => {
                writer.close().await?;
                return Ok(());
            }
            _ => {
                writer.send(message).await?;
            }
        }
    }
    Ok(())
}
