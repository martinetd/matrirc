use anyhow::{Error, Result};
use async_trait::async_trait;
use matrix_sdk::{
    room::Room,
    ruma::events::room::message::{MessageType, RoomMessageEventContent},
    RoomState,
};

use crate::matrix::room_mappings::{MatrixMessageType, MessageHandler, RoomTarget};

#[async_trait]
impl MessageHandler for Room {
    async fn handle_message(&self, message_type: MatrixMessageType, message: String) -> Result<()> {
        if self.state() != RoomState::Joined {
            Err(Error::msg(format!(
                "Room {} was not joined",
                self.room_id()
            )))?;
        };
        let content = match message_type {
            MatrixMessageType::Text => RoomMessageEventContent::text_plain(message),
            MatrixMessageType::Emote => RoomMessageEventContent::new(MessageType::new(
                "m.emote",
                message,
                serde_json::map::Map::new(),
            )?),
            MatrixMessageType::Notice => RoomMessageEventContent::notice_plain(message),
        };
        self.send(content).await?;
        Ok(())
    }
    // can't remove room from irc, we don't want (and can't anyway) keep target in room
    async fn set_target(&self, _target: RoomTarget) {}
}
