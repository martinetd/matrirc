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

use anyhow::{Result, Context};
use irc::client::prelude::*;
//use futures::Sink;
use futures::prelude::*;
use irc::proto::IrcCodec;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::{Decoder, Framed};

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
    let mut listener = TcpListener::bind("[::1]:6667").await?;
    info!("listening on localhost:6667");
    let accept_task = tokio::spawn(async move {
        while let Ok((socket, addr)) = listener.accept().await {
          info!("accepted connection from {}", addr);
          let codec = IrcCodec::new("utf-8").unwrap();
          let stream = codec.framed(socket);
          tokio::spawn(async move {
            handle_irc(stream).await
          });
        }
        info!("listener died");
    });
    accept_task.await?;
    Ok(())
}

async fn handle_irc(stream: Framed<TcpStream, IrcCodec>) -> Result<()> {
    let (mut writer, mut reader) = stream.split();
    while let Some(Ok(event)) = reader.next().await {
        match event.command {
            Command::PASS(e) => info!("got pass {}", e),
            _ => info!("got msg {:?}", event),
        }
    }
    Ok(())
}
