use anyhow::Result;
use log::{info, trace};
use matrix_sdk::{
    event_handler::Ctx,
    room::Room,
    ruma::events::room::member::{MembershipChange, OriginalSyncRoomMemberEvent},
    RoomState,
};

use crate::ircd::proto::IrcMessageType;
use crate::matrirc::Matrirc;

pub async fn on_room_member(
    event: OriginalSyncRoomMemberEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // ignore events from our own client (transaction set)
    if event.unsigned.transaction_id.is_some() {
        trace!("Ignored member event with transaction id (coming from self)");
        return Ok(());
    };
    // ignore non-joined rooms
    if room.state() != RoomState::Joined {
        trace!("Ignored member event in non-joined room");
        return Ok(());
    };

    trace!("Processing event {:?} to room {}", event, room.room_id());
    let target = matrirc.mappings().room_target(&room).await;

    let user = &event.sender;
    info!("Ok test user {}", user);

    let prev = event.unsigned.prev_content;

    let mchange = event.content.membership_change(
        prev.as_ref().map(|c| c.details()),
        &event.sender,
        &event.state_key,
    );
    info!("changed {:?}", mchange);
    match mchange {
        MembershipChange::Invited => {
            trace!(
                "{:?} was invited to {} by {}",
                event.content.displayname,
                target.target().await,
                event.sender
            );
            target
                .send_text_to_irc(
                    matrirc.irc(),
                    IrcMessageType::Notice,
                    &event.sender.into(),
                    format!(
                        "<invited {}>",
                        event
                            .content
                            .displayname
                            .unwrap_or_else(|| "???".to_string())
                    ),
                )
                .await?;
        }
        MembershipChange::Joined | MembershipChange::InvitationAccepted => {
            target
                .member_join(matrirc.irc(), event.sender, event.content.displayname)
                .await?;
        }
        MembershipChange::Left => {
            target.member_part(matrirc.irc(), event.sender).await?;
        }
        _ => (),
    }

    Ok(())
}
