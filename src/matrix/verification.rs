use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use log::warn;
use matrix_sdk::{
    encryption::verification::{
        format_emojis, SasState, SasVerification, Verification, VerificationRequest,
        VerificationRequestState,
    },
    event_handler::Ctx,
    ruma::{events::key::verification::request::ToDeviceKeyVerificationRequestEvent, UserId},
};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::matrirc::Matrirc;
use crate::matrix::room_mappings::{MatrixMessageType, MessageHandler, RoomTarget};

#[derive(Clone)]
struct VerificationContext {
    inner: Arc<RwLock<VerificationContextInner>>,
}
struct VerificationContextInner {
    matrirc: Matrirc,
    request: VerificationRequest,
    sas: Option<SasVerification>,
    target: Option<RoomTarget>,
    step: VerifState,
    stop: bool,
}
#[derive(Clone)]
enum VerifState {
    ConfirmStart,
    WaitingSas,
    ConfirmEmoji,
    WaitingDone,
}

impl VerificationContext {
    fn new(matrirc: Matrirc, request: VerificationRequest) -> Self {
        VerificationContext {
            inner: Arc::new(RwLock::new(VerificationContextInner {
                matrirc,
                request,
                sas: None,
                target: None,
                step: VerifState::ConfirmStart,
                stop: false,
            })),
        }
    }
    async fn to_irc<S: Into<String>>(&self, message: S) -> Result<()> {
        let guard = self.inner.read().await;
        guard
            .target
            .as_ref()
            .context("target should always be set")?
            .send_simple_query(guard.matrirc.irc(), message)
            .await
    }
    async fn stop(&self) -> Result<()> {
        let mut guard = self.inner.write().await;
        guard
            .matrirc
            .mappings()
            .remove_target(
                &guard
                    .target
                    .as_ref()
                    .context("target should always be set")?
                    .target()
                    .await,
            )
            .await;
        guard.stop = true;
        Ok(())
    }

    async fn sas_verification_handler_(&self, sas: SasVerification) -> Result<()> {
        self.inner.write().await.sas = Some(sas.clone());
        sas.accept().await?;
        let mut stream = sas.changes();
        while !self.inner.read().await.stop {
            let Some(state) = stream.next().await else {
                break;
            };
            // XXX add messages to irc?
            match state {
                SasState::KeysExchanged {
                    emojis,
                    decimals: _,
                } => {
                    let emojis = emojis.context("Only support verification with emojis")?;
                    self.inner.write().await.step = VerifState::ConfirmEmoji;
                    self.to_irc(format!(
                        "Got the following emojis:\n{}\nOk? [yes/no]",
                        format_emojis(emojis.emojis)
                    ))
                    .await?;
                }
                SasState::Done { .. } => {
                    let device = sas.other_device();

                    self.to_irc(format!(
                        "Successfully verified device {} {} {:?}",
                        device.user_id(),
                        device.device_id(),
                        device.local_trust_state()
                    ))
                    .await?;
                    self.stop().await?;
                    break;
                }
                SasState::Cancelled(cancel_info) => {
                    self.to_irc(format!(
                        "The verification has been cancelled, reason: {}",
                        cancel_info.reason()
                    ))
                    .await?;
                    break;
                }
                SasState::Started { .. } | SasState::Accepted { .. } | SasState::Confirmed => (),
            }
        }
        Ok(())
    }
    async fn sas_verification_handler(self, sas: SasVerification) {
        if let Err(e) = self.sas_verification_handler_(sas).await {
            let _ = self.to_irc(format!("Error handling sas: {}", e)).await;
        }
    }

    async fn request_verification_handler_(&self) -> Result<()> {
        let request = self.inner.read().await.request.clone();
        request.accept().await?;

        let mut stream = request.changes();

        while !self.inner.read().await.stop {
            let Some(state) = stream.next().await else {
                break;
            };
            // XXX add messages to irc?
            match state {
                VerificationRequestState::Created { .. }
                | VerificationRequestState::Requested { .. }
                | VerificationRequestState::Ready { .. } => (),
                VerificationRequestState::Transitioned { verification } => match verification {
                    Verification::SasV1(s) => {
                        tokio::spawn(self.clone().sas_verification_handler(s));
                        break;
                    }
                },
                VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => break,
            }
        }
        Ok(())
    }
    async fn request_verification_handler(self) {
        if let Err(e) = self.request_verification_handler_().await {
            let _ = self.to_irc(format!("Error handling verif: {}", e)).await;
        }
    }
    async fn handle_confirm_start(&self, message: String) -> Result<()> {
        match message.as_str() {
            "yes" => {
                self.to_irc("Ok, starting...").await?;
                self.inner.write().await.step = VerifState::WaitingSas;
                tokio::spawn(self.clone().request_verification_handler());
            }
            "no" => {
                let _ = self.to_irc("Ok, bye").await;
                self.stop().await?;
            }
            _ => {
                self.to_irc("Bad message, expecting yes or no").await?;
            }
        }
        Ok(())
    }
    async fn handle_confirm_emoji(&self, message: String) -> Result<()> {
        match message.as_str() {
            "yes" => {
                self.to_irc("Ok, accepting...").await?;
                self.inner.write().await.step = VerifState::WaitingDone;
                self.inner
                    .read()
                    .await
                    .sas
                    .as_ref()
                    .context("Sas should be set at this point")?
                    .confirm()
                    .await?;
            }
            "no" => {
                let _ = self.to_irc("Ok, aborting").await;
                self.inner
                    .read()
                    .await
                    .sas
                    .as_ref()
                    .context("Sas should be set at this point")?
                    .cancel()
                    .await?;
                self.stop().await?;
            }
            _ => {
                self.to_irc("Bad message, expecting yes or no").await?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl MessageHandler for VerificationContext {
    async fn handle_message(
        &self,
        _message_type: MatrixMessageType,
        message: String,
    ) -> Result<()> {
        let state = &self.inner.read().await.step.clone();
        match state {
            VerifState::ConfirmStart => self.handle_confirm_start(message).await,
            VerifState::ConfirmEmoji => self.handle_confirm_emoji(message).await,
            _ => {
                self.to_irc("not expecting any message at this point".to_string())
                    .await
            }
        }
    }

    async fn set_target(&self, target: RoomTarget) {
        self.inner.write().await.target = Some(target)
    }
}

pub async fn on_device_key_verification_request(
    event: ToDeviceKeyVerificationRequestEvent,
    matrirc: Ctx<Matrirc>,
) {
    if let Err(e) =
        handle_verification_request(&matrirc, &event.sender, &event.content.transaction_id).await
    {
        warn!("Verif failed: {}", e);
    }
}

pub async fn handle_verification_request(
    matrirc: &Matrirc,
    sender: &UserId,
    event_id: impl AsRef<str>,
) -> Result<()> {
    let request = matrirc
        .matrix()
        .encryption()
        .get_verification_request(sender, event_id)
        .await
        .context("Could not find verification request")?;
    let verif = VerificationContext::new(matrirc.clone(), request);
    matrirc.mappings().insert_deduped("verif", &verif).await;
    verif
        .to_irc(format!(
            "Got a verification request from {}, accept? [yes/no]",
            sender
        ))
        .await?;
    Ok(())
}
