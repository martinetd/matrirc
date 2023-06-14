use anyhow::Result;
use chrono::{offset::Local, DateTime};
use log::{info, trace};
use matrix_sdk::{
    config::SyncSettings,
    event_handler::Ctx,
    room::Room,
    ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent},
    ruma::MilliSecondsSinceUnixEpoch,
    LoopCtrl,
};
use std::time::SystemTime;

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
        trace!("Ignored message with transaction id (coming from self)");
        return Ok(());
    };
    // ignore non-joined rooms
    let Room::Joined(_) = room else {
        trace!("Ignored message in non-joined room");
        return Ok(())
    };

    trace!("Processing event {:?} to room {}", event, room.room_id());
    let target = matrirc.mappings().room_target(&room).await;

    let time_prefix = if MilliSecondsSinceUnixEpoch::now()
        .as_secs()
        .checked_sub(10u8.into())
        .unwrap_or(0u8.into())
        > event.origin_server_ts.as_secs()
    {
        let datetime: DateTime<Local> = event
            .origin_server_ts
            .to_system_time()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .into();
        datetime.format("<%Y-%m-%d %H:%M:%S> ").to_string()
    } else {
        "".to_string()
    };

    match event.content.msgtype {
        MessageType::Text(text_content) => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::PRIVMSG,
                    &event.sender,
                    time_prefix + &text_content.body,
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
