use anyhow::{Context, Error, Result};
use irc::{client::prelude::Command, proto::IrcCodec};
use crate::ircd::proto::{join, raw_msg};
use log::{debug, info, trace, warn};
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_util::codec::Framed;
// for Framed.tryNext()
// Note there's also a StreamExt in tokio-stream which covers
// streams, but we it's not the same and we don't care about the
// difference here
use futures::{SinkExt, TryStreamExt};
use matrix_sdk::{
    ruma::api::client::session::get_login_types::v3::LoginType, Client as MatrixClient,
};

use crate::{ircd::proto, matrix, state};

pub async fn auth_loop(
    stream: &mut Framed<TcpStream, IrcCodec>,
) -> Result<(String, String, MatrixClient)> {
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
        return Err(Error::msg("nick or pass wasn't set for client!"));
    };
    // need this to be able to interact with irssi: send welcome before any
    // privmsg exchange even if login isn't over.
    stream
        .send(proto::raw_msg(format!(
            ":matrirc 001 {} :Welcome to matrirc",
            nick
        )))
        .await?;
    info!("Processing login from {}!{}", nick, user);
    // Promote matric to chan
    let matrircchan = "matrirc".to_string();
    stream
        .send(join(
            Some(format!("{}!{}@matrirc", nick, user)),
            "matrirc".to_string(),
        ))
        .await?;
    stream
        .send(join(
            Some(format!("{}!{}@matrirc", nick, user)),
            "matrirc".to_string(),
        ))
        .await?;
    stream.send(raw_msg(format!(":matrirc 353 {} = {} :@matrirc", nick, matrircchan))).await?;
    stream.send(raw_msg(format!(":matrirc 366 {} {} :End", nick, matrircchan))).await?;
    let client = match state::login(&nick, &pass)? {
        Some(session) => matrix_restore_session(stream, &nick, &pass, session).await?,
        None => matrix_login_loop(stream, &nick, &pass).await?,
    };
    Ok((nick, user, client))
}

/// equivalent to ruma's LoginType, we need our own type for partialeq later
#[derive(Debug, PartialEq)]
enum LoginChoice {
    Password,
    Sso(Option<String>),
}

enum LoginFlow {
    /// just connected
    Init,
    /// got homeserver, letting user pick auth method
    Homeserver(String, MatrixClient, Vec<LoginChoice>),
    /// Done, login types is no longer used but
    Complete(String, MatrixClient),
}

struct LoginState<'a> {
    stream: &'a mut Framed<TcpStream, IrcCodec>,
    nick: &'a str,
    irc_pass: &'a str,
}

async fn matrix_login_choices(
    state: &mut LoginState<'_>,
    client: MatrixClient,
    homeserver: &str,
) -> Result<LoginFlow> {
    debug!("Querying {} auths", homeserver);
    let login_types = client.matrix_auth().get_login_types().await?.flows;

    state
        .stream
        .send(proto::privmsg(
            "matrirc",
            state.nick,
            format!(
                "Found server at {}; complete connection with one the following:",
                &homeserver
            ),
        ))
        .await?;

    state
        .stream
        .send(proto::privmsg("matrirc", state.nick, "reset (start over)"))
        .await?;

    let mut choices = vec![];
    for login_type in &login_types {
        debug!("Got login_type {:?}", login_type);
        match login_type {
            LoginType::Password(_) => {
                choices.push(LoginChoice::Password);
                state
                    .stream
                    .send(proto::privmsg(
                        "matrirc",
                        state.nick,
                        "password <user> <pass>",
                    ))
                    .await?
            }
            LoginType::Sso(sso) => {
                if sso.identity_providers.is_empty() {
                    choices.push(LoginChoice::Sso(None));
                    state
                        .stream
                        .send(proto::privmsg("matrirc", state.nick, "sso"))
                        .await?;
                } else {
                    for idp in &sso.identity_providers {
                        choices.push(LoginChoice::Sso(Some(idp.id.clone())));
                        state
                            .stream
                            .send(proto::privmsg(
                                "matrirc",
                                state.nick,
                                format!("sso {}", &idp.id),
                            ))
                            .await?;
                    }
                }
            }
            _ => (),
        }
    }
    // XXX reset immediately if choices.is_empty() ?
    Ok(LoginFlow::Homeserver(
        homeserver.to_string(),
        client,
        choices,
    ))
}

async fn matrix_login_password(
    state: &mut LoginState<'_>,
    client: MatrixClient,
    homeserver: &str,
    user: &str,
    pass: &str,
) -> Result<LoginFlow> {
    state
        .stream
        .send(proto::privmsg(
            "matrirc",
            state.nick,
            format!("Attempting to login to {} with {}", homeserver, user),
        ))
        .await?;
    debug!("Logging in to matrix for {} (user {})", state.nick, user);
    client
        .matrix_auth()
        .login_username(user, pass)
        .initial_device_display_name("matrirc")
        .send()
        .await?;
    Ok(LoginFlow::Complete(homeserver.to_string(), client))
}

async fn matrix_login_sso(
    state: &mut LoginState<'_>,
    homeserver: String,
    client: MatrixClient,
    choices: Vec<LoginChoice>,
    idp: Option<&str>,
) -> Result<LoginFlow> {
    // XXX check with &str somehow?
    if !choices.contains(&LoginChoice::Sso(idp.map(str::to_string))) {
        state
            .stream
            .send(proto::privmsg(
                "matrirc",
                state.nick,
                "invalid idp for sso, try again",
            ))
            .await?;
        return Ok(LoginFlow::Homeserver(homeserver, client, choices));
    }

    let (url_tx, url_rx) = oneshot::channel();
    let mut login_builder = client.matrix_auth().login_sso(|url| async move {
        if let Err(e) = url_tx.send(url) {
            warn!("Could not send privmsg: {e:?}");
        }
        Ok(())
    });

    if let Some(idp) = idp {
        login_builder = login_builder.identity_provider_id(idp);
    }

    let builder_future = tokio::spawn(async move { login_builder.await });

    match url_rx.await {
        Ok(url) => {
            state
                .stream
                .send(proto::privmsg(
                    "matrirc",
                    state.nick,
                    format!("Login at this URL: {url}"),
                ))
                .await?
        }
        Err(e) => {
            state
                .stream
                .send(proto::privmsg(
                    "matrirc",
                    state.nick,
                    format!("Could not get login url: {e:?}"),
                ))
                .await?;
            return Ok(LoginFlow::Homeserver(homeserver, client, choices));
            // let spawned task finish in background...
        }
    }
    builder_future.await??;

    Ok(LoginFlow::Complete(homeserver, client))
}

async fn matrix_login_state(
    state: &mut LoginState<'_>,
    flow: LoginFlow,
    message: String,
) -> Result<LoginFlow> {
    match flow {
        LoginFlow::Init => {
            // accept either single word (homeserver) or three words (homeserver user pass)
            match &message.split(' ').collect::<Vec<&str>>()[..] {
                [homeserver] => {
                    let client =
                        matrix::login::client(homeserver, state.nick, state.irc_pass).await?;
                    matrix_login_choices(state, client, homeserver).await
                }
                [homeserver, user, pass] => {
                    let client =
                        matrix::login::client(homeserver, state.nick, state.irc_pass).await?;
                    matrix_login_password(state, client, homeserver, user, pass).await
                }
                _ => {
                    state
                        .stream
                        .send(proto::privmsg(
                            "matrirc",
                            state.nick,
                            "Message not in <homeserver> [<user> <pass>] format, ignoring.",
                        ))
                        .await?;
                    Ok(LoginFlow::Init)
                }
            }
        }
        LoginFlow::Homeserver(homeserver, client, choices) => {
            match &message.split(' ').collect::<Vec<&str>>()[..] {
                ["reset"] => {
                    state
                        .stream
                        .send(proto::privmsg(
                            "matrirc",
                            state.nick,
                            "Start over from: <homeserver> [<user> <pass>]",
                        ))
                        .await?;
                    Ok(LoginFlow::Init)
                }
                ["password", user, pass] | ["pass", user, pass]
                    if choices.contains(&LoginChoice::Password) =>
                {
                    matrix_login_password(state, client, &homeserver, user, pass).await
                }
                ["sso"] => matrix_login_sso(state, homeserver, client, choices, None).await,
                ["sso", idp] => {
                    matrix_login_sso(state, homeserver, client, choices, Some(idp)).await
                }
                _ => {
                    state
                        .stream
                        .send(proto::privmsg(
                            "matrirc",
                            state.nick,
                            "Message not in recognized login format, try again (or 'reset')",
                        ))
                        .await?;
                    Ok(LoginFlow::Init)
                }
            }
        }
        _ => Err(Error::msg("Should never be called with complete type")),
    }
}

async fn matrix_login_loop(
    stream: &mut Framed<TcpStream, IrcCodec>,
    nick: &str,
    irc_pass: &str,
) -> Result<MatrixClient> {
    stream.send(proto::privmsg(
        "matrirc",
        nick,
        "Welcome to matrirc. Please login to matrix by replying with: <homeserver> [<user> <pass>]",
    ))
    .await?;
    let mut state = LoginState {
        stream,
        nick,
        irc_pass,
    };
    let mut flow = LoginFlow::Init;
    while let Some(event) = state.stream.try_next().await? {
        trace!("matrix connection loop: got {:?}", event);
        match event.command {
            Command::PING(server, server2) => {
                state.stream.send(proto::pong(server, server2)).await?
            }
            Command::PRIVMSG(_, body) => {
                flow = match matrix_login_state(&mut state, flow, body).await {
                    Ok(LoginFlow::Complete(homeserver, client)) => {
                        state::create_user(
                            nick,
                            irc_pass,
                            &homeserver,
                            client.session().context("client has no auth session")?,
                        )?;
                        return Ok(client);
                    }
                    Ok(f) => f,
                    Err(e) => {
                        debug!("Login error: {e:?}");
                        let mut err_string = e.to_string();
                        if err_string == "Building matrix client" {
                            err_string = format!(
                                "Could not build client: {}",
                                e.chain()
                                    .nth(1)
                                    .map(|e| e.to_string())
                                    .unwrap_or_else(|| "???".to_string())
                            )
                        }
                        state
                            .stream
                            .send(proto::privmsg(
                                "matrirc",
                                nick,
                                format!("Error: {err_string}."),
                            ))
                            .await?;
                        state
                            .stream
                            .send(proto::privmsg(
                                "matrirc",
                                nick,
                                "Try again from <homeserver> [<user> <pass>]",
                            ))
                            .await?;
                        LoginFlow::Init
                    }
                };
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
) -> Result<MatrixClient> {
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
