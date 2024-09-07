use anyhow::Result;
use log::trace;
use matrix_sdk::{
    event_handler::Ctx,
    room::Room,
    ruma::events::{
        reaction::OriginalSyncReactionEvent, room::message::MessageType,
        room::redaction::OriginalSyncRoomRedactionEvent, AnyMessageLikeEvent, AnyTimelineEvent,
        MessageLikeEvent,
    },
    ruma::EventId,
    RoomState,
};

use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;
use crate::matrix::time::ToLocal;

// OriginalRoomRedactionEvent for redactions
pub fn message_like_to_str(event: &AnyMessageLikeEvent) -> String {
    let AnyMessageLikeEvent::RoomMessage(event) = event else {
        return "(not a message)".to_string();
    };
    let MessageLikeEvent::Original(event) = event else {
        return "(redacted)".to_string();
    };

    match &event.content.msgtype {
        MessageType::Text(text_content) => text_content.body.clone(),
        MessageType::Emote(emote_content) => format!("emote: {}", emote_content.body),
        MessageType::Notice(notice_content) => notice_content.body.clone(),
        MessageType::ServerNotice(snotice_content) => snotice_content.body.clone(),
        MessageType::File(file_content) => format!("file: {}", &file_content.body),
        MessageType::Image(image_content) => format!("image: {}", &image_content.body,),
        MessageType::Video(video_content) => format!("video: {}", &video_content.body,),
        MessageType::VerificationRequest(_verif_content) => "(verification request)".to_string(),
        msg => {
            let data = if !msg.data().is_empty() {
                " (has data)"
            } else {
                ""
            };
            format!("{}{}: {}", msg.msgtype(), data, msg.body())
        }
    }
}
async fn get_message_from_event_id(
    matrirc: &Matrirc,
    room: &Room,
    event_id: &EventId,
) -> Result<String> {
    if let Some(message) = matrirc.message_get(event_id).await {
        return Ok(message);
    };
    let raw_event = room.event(event_id).await?;

    Ok(match raw_event.event.deserialize()? {
        AnyTimelineEvent::MessageLike(m) => {
            trace!("Got related message event: {:?}", m);

            let message = message_like_to_str(&m);
            format!(
                "message from {} @ {}: {}",
                m.sender(),
                m.origin_server_ts()
                    .localtime()
                    .unwrap_or_else(|| "just now".to_string()),
                message
            )
        }
        AnyTimelineEvent::State(s) => {
            trace!("Got related state event: {:?}", s);

            format!(
                "not a message from {} @ {}",
                s.sender(),
                s.origin_server_ts()
                    .localtime()
                    .unwrap_or_else(|| "just now".to_string()),
            )
        }
    })
    //match event {
    // happy path:
    // AnyTimelineEvent
    // MessageLike(AnyMessageLikeEvent),
    // (for redaction of reactions...) Reaction(ReactionEvent),
    // RoomMessage(RoomMessageEvent),
    // RoomMessageEvent = MessageLikeEvent<RoomMessageEventContent>;
    // same as sync_room_message...
}

pub async fn on_sync_reaction(
    event: OriginalSyncReactionEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // ignore events from our own client (transaction set)
    if event.unsigned.transaction_id.is_some() {
        trace!("Ignored reaction with transaction id (coming from self)");
        return Ok(());
    };
    // ignore non-joined rooms
    if room.state() != RoomState::Joined {
        trace!("Ignored reaction in non-joined room");
        return Ok(());
    };

    trace!(
        "Processing reaction event {:?} to room {}",
        event,
        room.room_id()
    );
    let target = matrirc.mappings().room_target(&room).await;

    let time_prefix = event
        .origin_server_ts
        .localtime()
        .map(|d| format!("<{}> ", d))
        .unwrap_or_default();
    let reaction = event.content.relates_to;
    let reaction_text = emoji::lookup_by_glyph::lookup(&reaction.key)
        .map(|e| format!("{} ({})", reaction.key, e.name))
        .unwrap_or(reaction.key.clone());
    let reacting_to = match get_message_from_event_id(&matrirc, &room, &reaction.event_id).await {
        Err(e) => format!("<Could not retreive: {}>", e),
        Ok(m) => m,
    };
    let message = format!(
        "{}<Reacted to {}>: {}",
        time_prefix, reacting_to, reaction_text
    );
    matrirc
        .message_put(event.event_id.clone(), message.clone())
        .await;
    // get error if any (warn/matrirc channel?)
    target
        .send_text_to_irc(
            matrirc.irc(),
            IrcMessageType::Privmsg,
            &event.sender.into(),
            message,
        )
        .await?;

    Ok(())
}
pub async fn on_sync_room_redaction(
    event: OriginalSyncRoomRedactionEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // ignore events from our own client (transaction set)
    if event.unsigned.transaction_id.is_some() {
        trace!("Ignored reaction with transaction id (coming from self)");
        return Ok(());
    };
    // ignore non-joined rooms
    if room.state() != RoomState::Joined {
        trace!("Ignored reaction in non-joined room");
        return Ok(());
    };

    trace!(
        "Processing redaction event {:?} to room {}",
        event,
        room.room_id()
    );
    let target = matrirc.mappings().room_target(&room).await;

    let time_prefix = event
        .origin_server_ts
        .localtime()
        .map(|d| format!("<{}> ", d))
        .unwrap_or_default();
    let reason = event.content.reason.as_deref().unwrap_or("(no reason)");
    let reacting_to = {
        match &event.redacts {
            None => "<Could not retreive: no redacted event id>".to_string(),
            Some(redacts) => match get_message_from_event_id(&matrirc, &room, redacts).await {
                Err(e) => format!("<Could not retreive: {}>", e),
                Ok(m) => m,
            },
        }
    };
    // get error if any (warn/matrirc channel?)
    target
        .send_text_to_irc(
            matrirc.irc(),
            IrcMessageType::Privmsg,
            &event.sender.into(),
            format!("{}<Redacted {}>: {}", time_prefix, reacting_to, reason),
        )
        .await?;

    Ok(())
}
