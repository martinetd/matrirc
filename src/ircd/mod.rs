use anyhow::{Context, Error, Result};
use irc::{client::prelude::Command, proto::IrcCodec};
use log::{debug, info};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
// for Framed.send()
//use futures::{SinkExt, StreamExt};
// for Framed.next()
use tokio_stream::StreamExt;

use crate::args::args;

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
            info!("Terminating {}: {}", addr, e)
        }
    });
    Ok(())
}

async fn handle_client(stream: Framed<TcpStream, IrcCodec>) -> Result<()> {
    debug!("Awaiting auth");
    let irc_nick = irc_auth_loop(stream).await?;
    info!("Login from {}", irc_nick);
    Ok(())
}

async fn irc_auth_loop(mut stream: Framed<TcpStream, IrcCodec>) -> Result<String> {
    let mut client_nick = None;
    while let Some(event) = stream.try_next().await? {
        match event.command {
            Command::NICK(nick) => client_nick = Some(nick),
            _ => {
                return Err(Error::msg(format!(
                    "Unexpected preauth message {:?}",
                    event
                )))
            }
        }
    }
    return match client_nick {
        Some(nick) => Ok(nick),
        None => Err(Error::msg("No nick for client!")),
    };
}
