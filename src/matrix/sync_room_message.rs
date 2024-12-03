use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use log::{info, trace, warn};
use matrix_sdk::{
    event_handler::Ctx,
    media::{MediaFormat, MediaRequestParameters},
    room::Room,
    ruma::events::room::{
        message::{MessageType, OriginalSyncRoomMessageEvent},
        MediaSource,
    },
    Client, RoomState,
};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::args::args;
use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;
use crate::matrix::time::ToLocal;
use crate::matrix::verification::handle_verification_request;

/// https://url.spec.whatwg.org/#fragment-percent-encode-set
const FRAGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'"').add(b'<').add(b'>').add(b'`');

#[async_trait]
pub trait SourceUri {
    async fn to_uri(&self, client: &Client, body: &str) -> Result<String>;
}
#[async_trait]
impl SourceUri for MediaSource {
    async fn to_uri(&self, client: &Client, body: &str) -> Result<String> {
        match self {
            MediaSource::Plain(uri) => {
                let homeserver = client.homeserver();
                Ok(uri.as_str().replace(
                    "mxc://",
                    &format!(
                        "{}/_matrix/media/r0/download/",
                        homeserver.as_str().trim_end_matches('/')
                    ),
                ))
            }
            _ => {
                let Some(dir_path) = &args().media_dir else {
                    return Err(Error::msg("<encrypted, no media dir set>"));
                };
                let media_request = MediaRequestParameters {
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
                Ok(format!(
                    "{}/{}",
                    url,
                    utf8_percent_encode(filename, FRAGMENT)
                ))
            }
        }
    }
}

async fn process_message_like_to_str(
    event: &OriginalSyncRoomMessageEvent,
    matrirc: &Matrirc,
) -> (String, IrcMessageType) {
    let time_prefix = event
        .origin_server_ts
        .localtime()
        .map(|d| format!("<{}> ", d))
        .unwrap_or_default();

    match &event.content.msgtype {
        MessageType::Text(text_content) => (
            time_prefix + text_content.body.as_str(),
            IrcMessageType::Privmsg,
        ),
        MessageType::Emote(emote_content) => (
            format!("\u{001}ACTION {}{}", time_prefix, emote_content.body),
            IrcMessageType::Privmsg,
        ),
        MessageType::Notice(notice_content) => (
            time_prefix + notice_content.body.as_str(),
            IrcMessageType::Notice,
        ),
        MessageType::ServerNotice(snotice_content) => (
            time_prefix + snotice_content.body.as_str(),
            IrcMessageType::Notice,
        ),
        MessageType::File(file_content) => {
            let url = file_content
                .source
                .to_uri(matrirc.matrix(), file_content.filename())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            (
                format!(
                    "{}Sent a file, {}: {}",
                    time_prefix, &file_content.body, url
                ),
                IrcMessageType::Notice,
            )
        }
        MessageType::Image(image_content) => {
            let url = image_content
                .source
                .to_uri(matrirc.matrix(), image_content.filename())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            (
                format!(
                    "{}Sent an image, {}: {}",
                    time_prefix, &image_content.body, url
                ),
                IrcMessageType::Notice,
            )
        }
        MessageType::Video(video_content) => {
            let url = video_content
                .source
                .to_uri(matrirc.matrix(), video_content.filename())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            (
                format!(
                    "{}Sent a video, {}: {}",
                    time_prefix, &video_content.body, url
                ),
                IrcMessageType::Notice,
            )
        }
        MessageType::Audio(audio_content) => {
            let url = audio_content
                .source
                .to_uri(matrirc.matrix(), audio_content.filename())
                .await
                .unwrap_or_else(|e| format!("{}", e));
            (
                format!(
                    "{}Sent audio, {}: {}",
                    time_prefix, &audio_content.body, url
                ),
                IrcMessageType::Notice,
            )
        }
        MessageType::VerificationRequest(verif_content) => {
            info!("Initiating verif content {:?}", verif_content);
            if let Err(e) =
                handle_verification_request(matrirc, &event.sender, &event.event_id).await
            {
                warn!("Verif failed: {}", e);
                (
                    format!(
                        "{}Sent a verification request, but failed: {}",
                        time_prefix, e
                    ),
                    IrcMessageType::Notice,
                )
            } else {
                (
                    format!("{}Sent a verification request", time_prefix),
                    IrcMessageType::Notice,
                )
            }
        }
        msg => {
            info!("Unhandled message: {:?}", event);
            let data = if !msg.data().is_empty() {
                " (has data)"
            } else {
                ""
            };
            (
                format!(
                    "{}Sent {}{}: {}",
                    time_prefix,
                    msg.msgtype(),
                    data,
                    msg.body()
                ),
                IrcMessageType::Privmsg,
            )
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
    if room.state() != RoomState::Joined {
        trace!("Ignored message in non-joined room");
        return Ok(());
    };

    trace!("Processing event {:?} to room {}", event, room.room_id());
    let target = matrirc.mappings().room_target(&room).await;

    let (message, message_type) = process_message_like_to_str(&event, &matrirc).await;
    matrirc
        .message_put(event.event_id.clone(), message.clone())
        .await;

    target
        .send_text_to_irc(matrirc.irc(), message_type, &event.sender.into(), message)
        .await?;

    Ok(())
}
