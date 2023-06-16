use anyhow::{Error, Result};
use async_trait::async_trait;
use matrix_sdk::{
    room::Room,
    ruma::events::room::message::{MessageType, RoomMessageEventContent},
};

use crate::matrix::room_mappings::{MatrixMessageType, MessageHandler};

#[async_trait]
impl MessageHandler for Room {
    async fn handle_message(&self, message_type: MatrixMessageType, message: String) -> Result<()> {
        if let Room::Joined(joined) = self {
            let content = match message_type {
                MatrixMessageType::Text => RoomMessageEventContent::text_plain(message),
                MatrixMessageType::Emote => RoomMessageEventContent::new(MessageType::new(
                    "m.emote",
                    message,
                    serde_json::map::Map::new(),
                )?),
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
