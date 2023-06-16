use anyhow::Result;
use matrix_sdk::{config::SyncSettings, LoopCtrl};

use crate::matrirc::Matrirc;

pub mod login;
mod outgoing;
pub mod room_mappings;
mod sync_room_message;

pub use room_mappings::MatrixMessageType;

pub async fn matrix_sync(matrirc: Matrirc) -> Result<()> {
    // add filter like with_lazy_loading() ?
    let sync_settings = SyncSettings::default();
    let client = matrirc.matrix();
    client.add_event_handler_context(matrirc.clone());
    client.add_event_handler(sync_room_message::on_room_message);

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
