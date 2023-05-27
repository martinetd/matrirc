use anyhow::{Context, Error};
use irc::proto::IrcCodec;
use log::info;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

use crate::args::args;

async fn handle_client(_stream: Framed<TcpStream, IrcCodec>) {
    println!("pretend we handled stream");
}

async fn handle_connection(socket: TcpStream) -> Result<(), Error> {
    let codec = IrcCodec::new("utf-8")?;
    let stream = Framed::new(socket, codec);
    tokio::spawn(async move { handle_client(stream).await });
    Ok(())
}

pub async fn listen() -> tokio::task::JoinHandle<()> {
    let args = args();

    info!("listening to {}", args.ircd_listen);
    let listener = TcpListener::bind(args.ircd_listen)
        .await
        .context("bind ircd port")
        .unwrap();
    tokio::spawn(async move {
        while let Ok((socket, addr)) = listener.accept().await {
            info!("Accepted connection from {}", addr);
            if let Err(e) = handle_connection(socket).await {
                info!("Could not spawn worker for {}: {}", addr, e);
            }
        }
    })
}
