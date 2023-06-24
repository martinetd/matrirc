use anyhow::Result;
use chrono::{offset::Local, DateTime};
use log::trace;
use matrix_sdk::{
    event_handler::Ctx,
    room::Room,
    ruma::events::{reaction::OriginalSyncReactionEvent, AnyTimelineEvent},
    ruma::EventId,
    ruma::MilliSecondsSinceUnixEpoch,
};
use std::time::SystemTime;

use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;

fn localtime_from_ts(ts: MilliSecondsSinceUnixEpoch) -> String {
    let datetime: DateTime<Local> = ts.to_system_time().unwrap_or(SystemTime::UNIX_EPOCH).into();
    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}
// OriginalRoomRedactionEvent for redactions

async fn get_message_from_event_id(room: &Room, event_id: &EventId) -> Result<String> {
    let raw_event = room.event(event_id).await?;

    Ok(match raw_event.event.deserialize()? {
        AnyTimelineEvent::MessageLike(m) => format!(
            "message from {} @ {}",
            m.sender(),
            localtime_from_ts(m.origin_server_ts()),
        ),
        AnyTimelineEvent::State(s) => format!(
            "not a message from {} @ {}",
            s.sender(),
            localtime_from_ts(s.origin_server_ts())
        ),
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
    let Room::Joined(_) = room else {
        trace!("Ignored reaction in non-joined room");
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

    let reaction = event.content.relates_to;
    let reaction_text = emoji::lookup_by_glyph::lookup(&reaction.key)
        .map(|e| format!("{} ({})", reaction.key, e.name))
        .unwrap_or(reaction.key.clone());
    let reacting_to = match get_message_from_event_id(&room, &reaction.event_id).await {
        Err(e) => format!("<Could not retreive: {}>", e),
        Ok(m) => m,
    };
    // get error if any (warn/matrirc channel?)
    target
        .send_text_to_irc(
            matrirc.irc(),
            IrcMessageType::Privmsg,
            &event.sender.into(),
            format!(
                "{}<Reacted to {}>: {}",
                time_prefix, reacting_to, reaction_text
            ),
        )
        .await?;

    Ok(())
}
