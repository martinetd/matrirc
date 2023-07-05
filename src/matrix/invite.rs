use anyhow::{Context, Result};
use async_trait::async_trait;
use log::{trace, warn};
use matrix_sdk::{
    event_handler::Ctx, room, room::Room, ruma::events::room::member::StrippedRoomMemberEvent,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

use crate::matrirc::Matrirc;
use crate::matrix::room_mappings::{room_name, MatrixMessageType, MessageHandler, RoomTarget};

#[derive(Clone)]
struct InvitationContext {
    inner: Arc<InvitationContextInner>,
}
struct InvitationContextInner {
    matrirc: Matrirc,
    room: room::Invited,
    room_name: String,
    target: RwLock<Option<RoomTarget>>,
}

impl InvitationContext {
    async fn new(matrirc: Matrirc, room: room::Invited) -> Self {
        InvitationContext {
            inner: Arc::new(InvitationContextInner {
                matrirc,
                room_name: room_name(&room).await,
                room,
                target: RwLock::new(None),
            }),
        }
    }
    async fn to_irc<S: Into<String>>(&self, message: S) -> Result<()> {
        let message: String = message.into();
        trace!("{}", &message);
        self.inner
            .target
            .read()
            .await
            .as_ref()
            .context("target should always be set")?
            .send_simple_query(self.inner.matrirc.irc(), message)
            .await
    }
    async fn stop(&self) -> Result<()> {
        self.inner
            .matrirc
            .mappings()
            .remove_target(
                &self
                    .inner
                    .target
                    .read()
                    .await
                    .as_ref()
                    .context("target should always be set")?
                    .target()
                    .await,
            )
            .await;
        Ok(())
    }
}

#[async_trait]
impl MessageHandler for InvitationContext {
    async fn handle_message(
        &self,
        _message_type: MatrixMessageType,
        message: String,
    ) -> Result<()> {
        match message.as_str() {
            "yes" => {
                let clone = self.clone();
                tokio::spawn(async move {
                    let room = clone.inner.room.clone();
                    if let Err(e) = clone
                        .to_irc(format!("Joining room {}", clone.inner.room_name))
                        .await
                    {
                        warn!("Couldn't send message: {}", e)
                    }
                    let mut delay = 2;
                    if let Some(joined) = loop {
                        match room.accept_invitation().await {
                            Ok(joined) => break Some(joined),
                            Err(err) => {
                                // example retries accepting a few times...
                                if delay > 1800 {
                                    let _ = clone
                                        .to_irc(format!(
                                            "Gave up joining room {}: {}",
                                            clone.inner.room_name, err
                                        ))
                                        .await;
                                    break None;
                                }
                                warn!(
                                    "Invite join room {} failed, retrying in {}: {}",
                                    clone.inner.room_name, delay, err
                                );
                                sleep(Duration::from_secs(delay)).await;
                                delay *= 2;
                            }
                        };
                    } {
                        let matrirc = &clone.inner.matrirc;
                        let new_target =
                            matrirc.mappings().room_target(&Room::Joined(joined)).await;
                        let _ = new_target
                            .send_simple_query(
                                matrirc.irc(),
                                format!("Joined room {}", clone.inner.room_name),
                            )
                            .await;
                    }
                    let _ = clone.stop().await;
                });
                ()
            }
            "no" => {
                self.to_irc("Okay").await?;
                // XXX log failure?
                self.inner.room.reject_invitation().await?;
                self.stop().await?;
            }
            _ => {
                self.to_irc("expecting yes or no").await?;
            }
        };
        Ok(())
    }

    async fn set_target(&self, target: RoomTarget) {
        *self.inner.target.write().await = Some(target)
    }
}

pub async fn on_stripped_state_member(
    room_member: StrippedRoomMemberEvent,
    room: Room,
    matrirc: Ctx<Matrirc>,
) -> Result<()> {
    // not for us
    if room_member.state_key
        != matrirc
            .matrix()
            .user_id()
            .context("Matrix client without user_id?")?
    {
        return Ok(());
    }
    // not an invite
    let Room::Invited(room) = room else {
        return Ok(());
    };
    let invite = InvitationContext::new(matrirc.clone(), room.clone()).await;
    matrirc.mappings().insert_deduped("invite", &invite).await;
    // XXX add reason and whatever else to message
    invite
        .to_irc(format!(
            "Got an invitation for {}, accept? [yes/no]",
            invite.inner.room_name
        ))
        .await?;
    Ok(())
}
