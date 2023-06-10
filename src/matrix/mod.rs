use anyhow::Result;
use log::info;
use matrix_sdk::{
    config::SyncSettings,
    event_handler::Ctx,
    room::Room,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent},
    LoopCtrl,
};

use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;

pub mod login;
pub mod room_mappings;

async fn on_room_message(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // ignore events from our own client (transaction set)
    if event.unsigned.transaction_id.is_some() {
        return Ok(());
    };
    // ignore non-joined rooms
    let Room::Joined(_) = room else { return Ok(()) };

    let target = matrirc.mappings().room_target(&room).await;

    match event.content.msgtype {
        MessageType::Text(text_content) => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::PRIVMSG,
                    &event.sender,
                    text_content.body,
                )
                .await?;
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
