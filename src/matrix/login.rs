use anyhow::Result;
use log::debug;
use matrix_sdk::{Client, Session};
use std::path::Path;

use crate::args::args;

pub async fn restore_session(
    homeserver: &str,
    session: Session,
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
        .login_username(user, pass)
        .initial_device_display_name("matrirc")
        .send()
        .await?;
    Ok(client)
}
