use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use matrix_sdk::{
    media::{MediaFormat, MediaRequest},
    room::Room,
    ruma::events::room::{
        message::{MessageType, RoomMessageEventContent},
        MediaSource,
    },
    Client,
};
use serde_json::map::Map;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::args::args;
use crate::matrix::room_mappings::MessageHandler;

pub enum MatrixMessageType {
    Text,
    Emote,
    Notice,
}

#[async_trait]
impl MessageHandler for Room {
    async fn handle_message(&self, message_type: MatrixMessageType, message: String) -> Result<()> {
        if let Room::Joined(joined) = self {
            let content = match message_type {
                MatrixMessageType::Text => RoomMessageEventContent::text_plain(message),
                MatrixMessageType::Emote => {
                    RoomMessageEventContent::new(MessageType::new("m.emote", message, Map::new())?)
                }
                MatrixMessageType::Notice => RoomMessageEventContent::notice_plain(message),
            };
            joined.send(content, None).await?;
            Ok(())
        } else {
            Err(Error::msg(format!(
                "Room {} was not joined",
                self.room_id()
            )))
        }
    }
}

#[async_trait]
pub trait SourceUri {
    async fn to_uri(&self, client: &Client, body: &String) -> Result<String>;
}
#[async_trait]
impl SourceUri for MediaSource {
    async fn to_uri(&self, client: &Client, body: &String) -> Result<String> {
        match self {
            MediaSource::Plain(uri) => {
                let homeserver = client.homeserver().await;
                Ok(uri.as_str().replace(
                    "mxc://",
                    &format!("{}/_matrix/media/r0/download/", homeserver.as_str()),
                ))
            }
            _ => {
                let Some(dir_path) = &args().media_dir else {
                    return Err(Error::msg("<encrypted, no media dir set>"))
                };
                let media_request = MediaRequest {
                    source: self.clone(),
                    format: MediaFormat::File,
                };
                let content = client
                    .media()
                    .get_media_content(&media_request, false)
                    .await
                    .context("Could not get decrypted data")?;
                let filename = body.rsplit_once('/').map(|(_, f)| f).unwrap_or(body);
                let dir = PathBuf::from(dir_path);
                if !dir.is_dir() {
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .recursive(true)
                        .create(&dir)
                        .await?
                }
                let file = dir.join(filename);
                fs::File::create(file).await?.write_all(&content).await?;
                let url = args().media_url.as_ref().unwrap_or(dir_path);
                Ok(format!("{}/{}", url, filename))
            }
        }
    }
}
