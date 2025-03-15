use anyhow::{Context, Result};
use log::debug;
use matrix_sdk::{
    authentication::matrix::{MatrixSession, MatrixSessionTokens},
    Client, SessionMeta,
};
use std::path::Path;

use crate::{args::args, state::SerializedMatrixSession};

pub async fn client(homeserver: &str, db_nick: &str, db_pass: &str) -> Result<Client> {
    let db_path = Path::new(&args().state_dir)
        .join(db_nick)
        .join("sqlite_store");
    debug!("Connection to matrix for {}", db_nick);
    // note: error 'Building matrix client' is matched as a string to get next error
    // to user on irc
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(db_path, Some(db_pass))
        .build()
        .await
        .context("Building matrix client")
}

pub async fn restore_session(
    homeserver: &str,
    serialized_session: SerializedMatrixSession,
    db_nick: &str,
    db_pass: &str,
) -> Result<Client> {
    let client = client(homeserver, db_nick, db_pass).await?;
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
