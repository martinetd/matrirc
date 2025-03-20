use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use irc::client::prelude::Message;
use irc::proto::IrcCodec;
use log::{debug, info};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::args::args;
use crate::matrirc::Matrirc;
use crate::matrix;

mod chan;
mod client;
mod login;
pub mod proto;

pub use chan::{join_irc_chan, join_irc_chan_finish};
pub use client::IrcClient;

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
            let _ = stream
                .send(proto::error(format!("Closing session: {}", e)))
                .await;
            return Err(e);
        }
    };
    info!("Authenticated {}!{}", nick, user);
    let (writer, reader_stream) = stream.split();
    let (irc_sink, irc_sink_rx) = mpsc::channel::<Message>(100);
    let irc = IrcClient::new(irc_sink, nick, user);
    let matrirc = Matrirc::new(matrix, irc);

    let writer_matrirc = matrirc.clone();
    tokio::spawn(async move {
        if let Err(e) = proto::ircd_sync_write(writer, irc_sink_rx).await {
            info!("irc write task failed: {:?}", e);
        } else {
            info!("irc write task done");
        }
        let _ = writer_matrirc.stop("irc writer task stopped").await;
    });

    let matrix_matrirc = matrirc.clone();
    tokio::spawn(async move {
        if let Err(e) = matrix::matrix_sync(matrix_matrirc.clone()).await {
            info!("Error in matrix_sync: {:?}", e);
        } else {
            info!("Stopped matrix sync task");
        }
        let _ = matrix_matrirc.stop("matrix sync task stopped").await;
    });

    let reader_matrirc = matrirc.clone();
    matrirc
        .irc()
        .send_privmsg("matrirc", &matrirc.irc().nick, "okay")
        .await?;

    proto::join_channels(&matrirc).await?;

    if let Err(e) = proto::ircd_sync_read(reader_stream, reader_matrirc).await {
        info!("irc read task failed: {:?}", e);
    }
    matrirc.stop("Reached end of handle_client").await?;
    Ok(())
}
