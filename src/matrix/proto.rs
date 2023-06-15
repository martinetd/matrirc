use anyhow::{Error, Result};
use async_trait::async_trait;
use matrix_sdk::{
    room::Room,
    ruma::events::room::{
        message::{MessageType, RoomMessageEventContent},
        MediaSource,
    },
    Client,
};
use serde_json::map::Map;

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
    async fn to_uri(&self, client: &Client) -> Result<String>;
}
#[async_trait]
impl SourceUri for MediaSource {
    async fn to_uri(&self, client: &Client) -> Result<String> {
        match self {
            MediaSource::Plain(uri) => {
                let homeserver = client.homeserver().await;
                Ok(uri.as_str().replace(
                    "mxc://",
                    &format!("{}/_matrix/media/r0/download/", homeserver.as_str()),
                ))
            }
            MediaSource::Encrypted(_enc) => Err(Error::msg("<encrypted>")),
        }
    }
}
