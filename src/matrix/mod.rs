use anyhow::Result;
use lazy_static::lazy_static;
use log::info;
use matrix_sdk::{
    config::SyncSettings,
    event_handler::Ctx,
    room::Room,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent},
    LoopCtrl,
};
use regex::Regex;

use crate::matrirc::Matrirc;

pub mod login;

async fn on_room_message(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // ignore non-joined rooms
    let Room::Joined(room) = room else { return Ok(()) };
    let room_name = match room.display_name().await {
        Ok(room_name) => room_name.to_string(),
        Err(error) => {
            info!("Error getting room display name: {error}");
            // Fallback to the room ID.
            room.room_id().to_string()
        }
    };
    lazy_static! {
        static ref SANITIZE: Regex = Regex::new("[ !@]").unwrap();
    }
    let room_name = SANITIZE.replace_all(&room_name, "");

    match event.content.msgtype {
        MessageType::Text(text_content) => {
            matrirc
                .irc()
                .send_privmsg(room_name, &matrirc.irc().nick, text_content.body)
                .await?
        }
        _ => info!("Ignored event {:?}", event),
    }
    Ok(())
}

pub async fn matrix_sync(matrirc: Matrirc) -> Result<()> {
    // add filter like with_lazy_loading() ?
    let sync_settings = SyncSettings::default();
    let client = matrirc.matrix();
    client.add_event_handler_context(matrirc.clone());
    client.add_event_handler(on_room_message);

    let loop_matrirc = &matrirc.clone();
    client
        .sync_with_result_callback(sync_settings, |_| async move {
            if loop_matrirc.running().await {
                Ok(LoopCtrl::Continue)
            } else {
                Ok(LoopCtrl::Break)
            }
        })
        .await?;
    Ok(())
}
