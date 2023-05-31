use anyhow::{Context, Result};
use futures::StreamExt;
use irc::client::prelude::Message;
use irc::proto::IrcCodec;
use log::{debug, info};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::args::args;
use crate::matrirc::Matrirc;

pub mod client;
mod login;
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
    let (nick, user, matrix) = match login::auth_loop(&mut stream).await {
        Ok(data) => data,
        Err(e) => {
            // keep original error, but try to tell client we're not ok
            let _ = proto::send_raw_msg(&mut stream, format!("Closing session: {}", e)).await;
            return Err(e);
        }
    };
    info!("Authenticated {}!{}", nick, user);
    let (writer, _reader) = stream.split();
    let (irc_sink, irc_sink_rx) = mpsc::channel::<Message>(100);
    tokio::spawn(async move {
        if let Err(e) = proto::irc_write_thread(writer, irc_sink_rx).await {
            info!("irc write thread failed: {}", e);
        }
    });
    let irc = client::IrcClient::new(irc_sink);
    let matrirc = Matrirc::new(matrix, irc);
    // TODO
    // setup matrix handlers
    // spawn matrix sync while matrirc.running
    // listen to reader until socket closed
    // set running to false / send quit
    matrirc.irc.send_privmsg("matrirc", nick, "okay").await?;
    Ok(())
}
