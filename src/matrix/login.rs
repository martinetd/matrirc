use anyhow::Result;
use log::debug;
use matrix_sdk::{
    matrix_auth::{MatrixSession, MatrixSessionTokens},
    Client, SessionMeta,
};
use std::path::Path;

use crate::{args::args, state::SerializedMatrixSession};

pub async fn restore_session(
    homeserver: &str,
    serialized_session: SerializedMatrixSession,
    db_nick: &str,
    db_pass: &str,
) -> Result<Client> {
    let db_path = Path::new(&args().state_dir)
        .join(db_nick)
        .join("sqlite_store");
    debug!("Connection to matrix for {}", db_nick);
    let client = Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(db_path, Some(db_pass))
        .build()
        .await?;
    debug!("Restoring session for {}", db_nick);
    let session = MatrixSession {
        meta: SessionMeta {
            user_id: serialized_session.user_id.try_into()?,
            device_id: serialized_session.device_id.into(),
        },
        tokens: MatrixSessionTokens {
            access_token: serialized_session.access_token,
            refresh_token: serialized_session.refresh_token,
        },
    };
    client.restore_session(session).await?;
    Ok(client)
}

pub async fn login(
    homeserver: &str,
    user: &str,
    pass: &str,
    db_nick: &str,
    db_pass: &str,
) -> Result<Client> {
    let db_path = Path::new(&args().state_dir)
        .join(db_nick)
        .join("sqlite_store");
    debug!("Connection to matrix for {}", db_nick);
    let client = Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(db_path, Some(db_pass))
        .build()
        .await?;
    debug!("Logging in to matrix for {} (user {})", db_nick, user);
    client
        .matrix_auth()
        .login_username(user, pass)
        .initial_device_display_name("matrirc")
        .send()
        .await?;
    Ok(client)
}
