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
use std::collections::HashMap;

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
        SyncMessageEvent,
    },
    Client, ClientConfig, EventEmitter, SyncRoom, SyncSettings, Session
};
use matrix_sdk_common_macros::async_trait;
use matrix_sdk_common::identifiers::UserId;
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


struct EventCallback {
    client: Client,
    toirc: mpsc::Sender<Message>,
}

impl EventCallback {
    pub fn new(client: Client, toirc: mpsc::Sender<Message>) -> Self {
        Self { client, toirc }
    }
}

#[async_trait]
impl EventEmitter for EventCallback {
    async fn on_room_message(&self, room: SyncRoom, event: &SyncMessageEvent<MessageEventContent>) {
        if let SyncRoom::Joined(room) = room {
            if let SyncMessageEvent {
                content: MessageEventContent::Text(TextMessageEventContent { body: msg_body, .. }),
                sender,
                ..
            } = event
            {
                let (username, roomname) = {
                    // any reads should be held for the shortest time possible to
                    // avoid dead locks
                    let room = room.read().await;
                    let member = room.joined_members.get(&sender).unwrap();
                    (member.name(), room.display_name())
                };
                println!("{}: <{}> {}", roomname, username, msg_body);
                // XXX sanitize names
                // XXX if backlog (somehow check?) get and send timestamps
                self.toirc.clone().send(irc_raw_msg(format!(":{}!none@none PRIVMSG {} :{}", username, roomname, msg_body))).await.unwrap();
            }
        }
    }
}


async fn matrix_init(toirc: mpsc::Sender<Message>) -> matrix_sdk::Client {
    let homeserver = dotenv::var("HOMESERVER").context("HOMESERVER is not set").unwrap();
    let store_path = match dotenv::var("STORE_PATH") {
        Ok(path) => PathBuf::from(&path),
         _ => dirs::data_dir().and_then(|a| Some(a.join("matrirc/matrix_store"))).unwrap()
    };

    let client_config = ClientConfig::new().store_path(store_path);
    let homeserver_url = Url::parse(&homeserver).expect("Couldn't parse the homeserver URL");
    let mut client = Client::new_with_config(homeserver_url, client_config).unwrap();

    client.add_event_emitter(Box::new(EventCallback::new(client.clone(), toirc))).await;

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
/*fn irc_msg(prefix: Option<&str>, command: Command) -> Message {
    Message {
        tags: None,
        prefix: prefix.and_then(|p| { Some(Prefix::new_from_str(p)) }),
        command,
    }
}*/
fn irc_msg(command: Command) -> Message {
    Message {
        tags: None,
        prefix: None,
        command,
    }
}

fn irc_join_msg(irc_nick: &str, irc_user: &str, chan: &str) -> Message {
    Message {
        tags: None,
        prefix: Some(Prefix::new_from_str(&format!("{}!{}@none", irc_nick, irc_user))),
        command: Command::JOIN(chan.into(), None, None),
    }
}

async fn handle_irc(stream: Framed<TcpStream, IrcCodec>) -> Result<()> {
    // Check auth before doing anything else
    let (irc_nick, irc_user, stream) = irc_auth_loop(stream).await?;
    let (mut writer, mut reader) = stream.split();
    writer.send(irc_raw_msg(format!(":{} 001 {} :Welcome to matrirc", "matrirc", irc_nick))).await?;

    let (mut toirc, toirc_rx) = mpsc::channel::<Message>(100);
    let (inick, iuser) = (irc_nick.clone(), irc_user.clone());
    tokio::spawn(async move { irc_write_thread(&inick, &iuser, writer, toirc_rx).await });

    // setup matrix things
    let matrix_client = matrix_init(toirc.clone()).await;

    // We're now ready to handle the rest
    //writer.send(irc_raw_msg(format!(":{} MODE {} :+iw", irc_nick, irc_nick))).await?;

    // initial matrix sync required for room list
    matrix_client.sync(SyncSettings::new()).await?;

    // iterate channels and join if requested
    if let Ok(_) = dotenv::var("IRC_AUTOJOIN") {
        for room in matrix_client.joined_rooms().read().await.values() {
            toirc.send(irc_join_msg(&irc_nick, &irc_user, &room.read().await.display_name())).await?;
            // for user in room.joined_members.values() {
            //   :wolfe.freenode.net 332 asmatest #channame topic content
            //   :wolfe.freenode.net 333 asmatest #channame topicauthor 1389652509
            //   :wolfe.freenode.net 353 asmatest @ #channame :asmatest nick1 nick2 nick3
            //   :wolfe.freenode.net 366 asmatest #channame :End of /NAMES list.
            //   user.name()
            // }
        }
    }

    tokio::spawn(async move { matrix_client.sync_forever(SyncSettings::new(), |_| async {}).await });

    while let Some(Ok(event)) = reader.next().await {
        match event.command {
            Command::PING(e, o) => {
                // XXX if let Err(_) = ... { handle dropped
                toirc.send(irc_msg(Command::PONG(e, o))).await?;
            }
            Command::QUIT(_) => {
                toirc.send(irc_msg(Command::QUIT(None))).await?;
            }
            Command::JOIN(chan, _, _) => {
                toirc.send(irc_join_msg(&irc_nick, &irc_user, &chan)).await?;
            }
            _ => info!("got msg {:?}", event),
        }
    }

    // XXX cleanup matrix at this point or reconnect won't work
    info!("disconnect event");
    Ok(())
}

async fn irc_write_thread(irc_nick: &str, irc_user: &str, mut writer: futures::stream::SplitSink<Framed<TcpStream, IrcCodec>, Message>, mut toirc_rx: mpsc::Receiver<Message>) -> Result<()> {
    let mut joined_chans = HashMap::new();
    while let Some(message) = toirc_rx.recv().await {
        // XXX clone because partial move... ugh.
        match message.command.clone() {
            Command::QUIT(_) => {
                writer.close().await?;
                return Ok(());
            }
            Command::JOIN(chan, _, _) => {
                if !joined_chans.contains_key(&chan) {
                    joined_chans.insert(chan, 0);
                    writer.send(message).await?;
                }
            }
            Command::PRIVMSG(chan, _) => {
                if !joined_chans.contains_key(&chan) {
                    joined_chans.insert(chan.clone(), 0);
                    writer.send(irc_join_msg(&irc_nick, &irc_user, &chan)).await?;
                }
                writer.send(message).await?;
            }
            _ => {
                writer.send(message).await?;
            }
        }
    }
    Ok(())
}
