use anyhow::Result;
use log::warn;
use matrix_sdk::{config::SyncSettings, LoopCtrl};

use crate::matrirc::{Matrirc, Running};

mod invite;
pub mod login;
mod outgoing;
pub mod room_mappings;
mod sync_reaction;
mod sync_room_member;
mod sync_room_message;
pub mod time;
mod verification;

pub use room_mappings::MatrixMessageType;

pub async fn matrix_sync(matrirc: Matrirc) -> Result<()> {
    // add filter like with_lazy_loading() ?
    let sync_settings = SyncSettings::default();
    let client = matrirc.matrix();
    client.add_event_handler_context(matrirc.clone());
    client.add_event_handler(sync_room_message::on_room_message);
    client.add_event_handler(sync_reaction::on_sync_reaction);
    client.add_event_handler(sync_reaction::on_sync_room_redaction);
    client.add_event_handler(verification::on_device_key_verification_request);
    client.add_event_handler(invite::on_stripped_state_member);
    client.add_event_handler(sync_room_member::on_room_member);

    let loop_matrirc = &matrirc.clone();
    client
        .sync_with_result_callback(sync_settings, |_| async move {
            match loop_matrirc.running().await {
                Running::First => {
                    if let Err(e) = loop_matrirc.mappings().sync_rooms(loop_matrirc).await {
                        warn!("Got an error syncing rooms on first loop: {}", e);
                        // XXX send to irc
                        Ok(LoopCtrl::Break)
                    } else {
                        Ok(LoopCtrl::Continue)
                    }
                }
                Running::Continue => Ok(LoopCtrl::Continue),
                Running::Break => Ok(LoopCtrl::Break),
            }
        })
        .await?;
    Ok(())
}
