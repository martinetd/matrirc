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
use crate::matrix::proto::SourceUri;

pub mod login;
pub mod proto;
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
                    IrcMessageType::Privmsg,
                    &event.sender,
                    time_prefix + &text_content.body,
                )
                .await?;
        }
        MessageType::Emote(emote_content) => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Privmsg,
                    &event.sender,
                    format!("\u{001}ACTION {}{}", time_prefix, emote_content.body),
                )
                .await?;
        }
        MessageType::Notice(notice_content) => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    time_prefix + &notice_content.body,
                )
                .await?;
        }
        MessageType::ServerNotice(snotice_content) => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    time_prefix + &snotice_content.body,
                )
                .await?;
        }
        MessageType::File(file_content) => {
            let url = file_content
                .source
                .to_uri(matrirc.matrix())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    format!(
                        "{}Sent a file, {}: {}",
                        time_prefix, &file_content.body, url
                    ),
                )
                .await?;
        }
        MessageType::Image(image_content) => {
            let url = image_content
                .source
                .to_uri(matrirc.matrix())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    format!(
                        "{}Sent a image, {}: {}",
                        time_prefix, &image_content.body, url
                    ),
                )
                .await?;
        }
        MessageType::Video(video_content) => {
            let url = video_content
                .source
                .to_uri(matrirc.matrix())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    format!(
                        "{}Sent a video, {}: {}",
                        time_prefix, &video_content.body, url
                    ),
                )
                .await?;
        }
        MessageType::VerificationRequest(verif_content) => {
            info!("Initiating verif content {:?}", verif_content);
        }
        _ => {
            target
                .send_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender,
                    format!("{}<Unhandled message, check another client>", time_prefix),
                )
                .await?;
        }
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
