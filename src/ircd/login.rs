use anyhow::{Context, Error, Result};
use irc::{client::prelude::Command, proto::IrcCodec};
use log::{info, trace};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;
// for Framed.tryNext()
// Note there's also a StreamExt in tokio-stream which covers
// streams, but we it's not the same and we don't care about the
// difference here
use futures::{SinkExt, TryStreamExt};

use crate::{ircd::proto, matrix, state};

pub async fn auth_loop(
    stream: &mut Framed<TcpStream, IrcCodec>,
) -> Result<(String, String, matrix_sdk::Client)> {
    let mut client_nick = None;
    let mut client_user = None;
    let mut client_pass = None;
    while let Some(event) = stream.try_next().await? {
        trace!("auth loop: got {:?}", event);
        match event.command {
            Command::NICK(nick) => client_nick = Some(nick),
            Command::PASS(pass) => client_pass = Some(pass),
            Command::USER(user, _, _) => {
                client_user = Some(user);
                break;
            }
            Command::PING(server, server2) => stream.send(proto::pong(server, server2)).await?,
            Command::CAP(_, _, Some(code), _) => {
                // required for recent-ish versions of irssi
                if code == "302" {
                    stream.send(proto::raw_msg(":matrirc CAP * LS :")).await?;
                }
            }
            _ => (), // ignore
        }
    }

    let (Some(nick), Some(user), Some(pass)) = (client_nick, client_user, client_pass) else {
        return Err(Error::msg("nick or pass wasn't set for client!"))
    };
    // need this to be able to interact with irssi: send welcome before any
    // privmsg exchange even if login isn't over.
    stream
        .send(proto::raw_msg(format!(
            ":matrirc 001 {} :Welcome to matrirc",
            nick
        )))
        .await?;
    // XXX spawn a task to handle just ping? what about other messages from irc client, queue
    // somewhere?... note probably not safe to use tokio::select! or similar as
    // matrix_restore_session or login loop probably aren't cancel-safe
    info!("Processing login from {}!{}", nick, user);
    let client = match state::login(&nick, &pass)? {
        Some(session) => matrix_restore_session(stream, &nick, &pass, session).await?,
        None => matrix_login_loop(stream, &nick, &pass).await?,
    };
    Ok((nick, user, client))
}

async fn matrix_login_loop(
    stream: &mut Framed<TcpStream, IrcCodec>,
    nick: &str,
    irc_pass: &str,
) -> Result<matrix_sdk::Client> {
    stream.send(proto::privmsg(
        "matrirc",
        nick,
        "Welcome to matrirc. Please login to matrix by replying with: <homeserver> <user> <pass>",
    ))
    .await?;
    while let Some(event) = stream.try_next().await? {
        trace!("matrix connection loop: got {:?}", event);
        match event.command {
            Command::PING(server, server2) => stream.send(proto::pong(server, server2)).await?,
            Command::PRIVMSG(_, body) => {
                let [homeserver, user, pass] = body.splitn(3, ' ').collect::<Vec<&str>>()[..] else {
                    stream.send(proto::privmsg(
                        "matrirc",
                        nick,
                        "Message not in <homeserver> <user> <pass> format, ignoring.".to_string(),
                    ))
                    .await?;
                    continue;
                };
                stream
                    .send(proto::privmsg(
                        "matrirc",
                        nick,
                        format!("Attempting to login to {} with {}", homeserver, user),
                    ))
                    .await?;
                match matrix::login::login(homeserver, user, pass, nick, irc_pass).await {
                    Ok(client) => {
                        state::create_user(
                            nick,
                            irc_pass,
                            state::Session {
                                homeserver: homeserver.into(),
                                matrix_session: client
                                    .session()
                                    .context("client has no session")?,
                            },
                        )?;
                        return Ok(client);
                    }
                    Err(e) => {
                        stream
                            .send(proto::privmsg(
                                "matrirc",
                                nick,
                                format!("Login failed: {}. Try again.", e),
                            ))
                            .await?;
                        continue;
                    }
                }
            }
            _ => (), // ignore
        }
    }
    Err(Error::msg("Stream finished in matrix login loop?"))
}

async fn matrix_restore_session(
    stream: &mut Framed<TcpStream, IrcCodec>,
    nick: &str,
    irc_pass: &str,
    session: state::Session,
) -> Result<matrix_sdk::Client> {
    stream
        .send(proto::privmsg(
            "matrirc",
            nick,
            format!(
                "Welcome to matrirc. Restoring session to {}",
                session.homeserver
            ),
        ))
        .await?;
    match matrix::login::restore_session(
        &session.homeserver,
        session.matrix_session,
        nick,
        irc_pass,
    )
    .await
    {
        // XXX can't make TryFutureExt's or_else work, give up
        Ok(client) => Ok(client),
        Err(e) => {
            stream.send(proto::privmsg(
                "matrirc",
                nick,
                format!(
                    "Restoring session failed: {}. Login again as follow or try to reconnect later.",
                    e
                ),
            ))
            .await?;

            matrix_login_loop(stream, nick, irc_pass).await
        }
    }
}
