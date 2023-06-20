use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use chrono::{offset::Local, DateTime};
use log::{info, trace, warn};
use matrix_sdk::{
    event_handler::Ctx,
    media::{MediaFormat, MediaRequest},
    room::Room,
    ruma::events::room::{
        message::{MessageType, OriginalSyncRoomMessageEvent},
        MediaSource,
    },
    ruma::MilliSecondsSinceUnixEpoch,
    Client,
};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::args::args;
use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;

#[async_trait]
pub trait SourceUri {
    async fn to_uri(&self, client: &Client, body: &str) -> Result<String>;
}
#[async_trait]
impl SourceUri for MediaSource {
    async fn to_uri(&self, client: &Client, body: &str) -> Result<String> {
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

pub async fn on_room_message(
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

    match &event.content.msgtype {
        MessageType::Text(text_content) => {
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Privmsg,
                    &event.sender.into(),
                    time_prefix + &text_content.body,
                )
                .await?;
        }
        MessageType::Emote(emote_content) => {
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Privmsg,
                    &event.sender.into(),
                    format!("\u{001}ACTION {}{}", time_prefix, emote_content.body),
                )
                .await?;
        }
        MessageType::Notice(notice_content) => {
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
                    time_prefix + &notice_content.body,
                )
                .await?;
        }
        MessageType::ServerNotice(snotice_content) => {
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
                    time_prefix + &snotice_content.body,
                )
                .await?;
        }
        MessageType::File(file_content) => {
            let url = file_content
                .source
                .to_uri(matrirc.matrix(), &file_content.body)
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
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
                .to_uri(matrirc.matrix(), &image_content.body)
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
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
                .to_uri(matrirc.matrix(), &video_content.body)
                .await
                .unwrap_or_else(|e| format!("{}", e));
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
                    format!(
                        "{}Sent a video, {}: {}",
                        time_prefix, &video_content.body, url
                    ),
                )
                .await?;
        }
        MessageType::VerificationRequest(verif_content) => {
            info!("Initiating verif content {:?}", verif_content);
            // KeyVerificationRequestEventContent { body: "@x:y.z is requesting to verify your key, but your client does not support in-chat key verification.  You will need to use legacy key verification to verify keys.", formatted: None, methods: ["m.sas.v1", "m.qr_code.show.v1", "m.reciprocate.v1"], from_device: "AAAAAAAAAA", to: "@x2:y2.z2" }
        }
        msg => {
            info!("Unhandled message: {:?}", event);
            let data = if !msg.data().is_empty() {
                " (has data)"
            } else {
                ""
            };
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Privmsg,
                    &event.sender.into(),
                    format!(
                        "{}Sent {}{}: {}",
                        time_prefix,
                        msg.msgtype(),
                        data,
                        msg.body()
                    ),
                )
                .await?;
        }
    }
    Ok(())
}
