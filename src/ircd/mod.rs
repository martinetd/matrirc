use anyhow::{Context, Error, Result};
use irc::{client::prelude::Command, proto::IrcCodec};
use log::{debug, info, trace};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
// for Framed.tryNext()
// Note there's also a StreamExt in tokio-stream which covers
// streams, but we it's not the same and we don't care about the
// difference here
use futures::TryStreamExt;

use crate::args::args;
use crate::state;

mod proto;

pub async fn listen() -> tokio::task::JoinHandle<()> {
    info!("listening to {}", args().ircd_listen);
    let listener = TcpListener::bind(args().ircd_listen)
        .await
        .context("bind ircd port")
        .unwrap();
    tokio::spawn(async move {
        while let Ok((socket, addr)) = listener.accept().await {
            info!("Accepted connection from {}", addr);
            if let Err(e) = handle_connection(socket, addr).await {
                info!("Could not spawn worker: {}", e);
            }
        }
    })
}

async fn handle_connection(socket: TcpStream, addr: SocketAddr) -> Result<()> {
    let codec = IrcCodec::new("utf-8")?;
    let stream = Framed::new(socket, codec);
    tokio::spawn(async move {
        if let Err(e) = handle_client(stream).await {
            info!("Terminating {}: {}", addr, e);
        }
    });
    Ok(())
}

async fn handle_client(mut stream: Framed<TcpStream, IrcCodec>) -> Result<()> {
    debug!("Awaiting auth");
    let (nick, user, _pass) = match auth_loop(&mut stream).await {
        Ok(data) => data,
        Err(e) => {
            // keep original error, but try to tell client we're not ok
            let _ = proto::send_raw_msg(&mut stream, format!("Closing session: {}", e)).await;
            return Err(e);
        }
    };
    info!("Authenticated {}!{}", nick, user);
    Ok(())
}

async fn auth_loop(stream: &mut Framed<TcpStream, IrcCodec>) -> Result<(String, String, String)> {
    let mut client_nick = None;
    let mut client_user = None;
    let mut client_pass = None;
    while let Some(event) = stream.try_next().await? {
        trace!("auth loop: got {:?}", event);
        match event.command {
            Command::NICK(nick) => client_nick = Some(nick),
            Command::PASS(pass) => client_pass = Some(pass),
            Command::USER(user, _, _) => {
                client_user = Some(user);
                break;
            }
            Command::CAP(_, _, Some(code), _) => {
                // required for recent-ish versions of irssi
                if code == "302" {
                    proto::send_raw_msg(stream, ":matrirc CAP * LS :".to_string()).await?;
                }
            }
            _ => (), // ignore
        }
    }
    if let (Some(nick), Some(user), Some(pass)) = (client_nick, client_user, client_pass) {
        info!("Processing login from {}!{}", nick, user);
        match state::login(&nick, &pass)? {
            Some(_session) => {
                // restore matrix session
                Ok((nick, user, pass))
            }
            None => {
                // matrix login
                state::create_user(
                    &nick,
                    &pass,
                    state::Session {
                        homeserver: "test".to_string(),
                    },
                )?;
                Ok((nick, user, pass))
            }
        }
    } else {
        Err(Error::msg("nick or pass wasn't set for client!"))
    }
}
