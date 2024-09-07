use anyhow::{Context, Error, Result};
use argon2::{
    password_hash::rand_core::{OsRng, RngCore},
    Argon2,
};
use base64_serde::base64_serde_type;
use chacha20poly1305::{aead::Aead, KeyInit, XChaCha20Poly1305};
use log::info;
use matrix_sdk::AuthSession;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

base64_serde_type!(Base64, base64::engine::general_purpose::STANDARD);

use crate::args::args;

/// data we want to keep around
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub homeserver: String,
    pub matrix_session: SerializedMatrixSession,
}

/// matrix-rust-sdk's "Session" struct as we used to serialize it
/// as of matrix-rust-sdk commit 0b9c082e11955f49f99acd21542f62b40f11c418
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SerializedMatrixSession {
    /// The access token used for this session.
    pub access_token: String,
    /// The token used for [refreshing the access token], if any.
    ///
    /// [refreshing the access token]: https://spec.matrix.org/v1.3/client-server-api/#refreshing-access-tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// The user the access token was issued for.
    pub user_id: String,
    /// The ID of the client device.
    pub device_id: String,
}

/// data required for decryption
#[derive(serde::Serialize, serde::Deserialize)]
struct Blob {
    version: String,
    #[serde(with = "Base64")]
    ciphertext: Vec<u8>,
    #[serde(with = "Base64")]
    salt: Vec<u8>,
    #[serde(with = "Base64")]
    nonce: Vec<u8>,
}

/// try to decrypt session and return it
fn check_pass(session_file: PathBuf, pass: &str) -> Result<Session> {
    let blob = {
        let content = fs::read(session_file).context("Could not read user session file")?;
        serde_json::from_slice::<Blob>(&content)
            .context("Could not deserialize session file content.")?
    };
    if blob.version != "argon2+chacha20poly1305" {
        return Err(Error::msg(
            "This version only supports argon2+chacha20poly1305",
        ));
    }
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(pass.as_bytes(), &blob.salt, &mut key)
        .context("Could not hash password")?;
    let cipher = XChaCha20Poly1305::new(&key.into());
    let plaintext = cipher
        .decrypt(blob.nonce.as_slice().into(), &*blob.ciphertext)
        .map_err(|_| Error::msg("Could not decrypt blob: bad password?"))?;

    let session = serde_json::from_slice::<Session>(&plaintext)
        .context("Could not deserialize stored session")?;
    info!("Decrypted {}", session.homeserver);
    Ok(session)
}

/// encrypt session and store it
pub fn create_user(
    nick: &str,
    pass: &str,
    homeserver: &str,
    auth_session: AuthSession,
) -> Result<()> {
    let session_meta = auth_session.meta();
    let session = Session {
        homeserver: homeserver.into(),
        matrix_session: SerializedMatrixSession {
            access_token: auth_session.access_token().into(),
            refresh_token: auth_session.get_refresh_token().map(str::to_string),
            user_id: session_meta.user_id.as_str().into(),
            device_id: session_meta.device_id.as_str().into(),
        },
    };
    let mut key = [0u8; 32];
    let mut salt = vec![0u8; 32];
    let mut nonce = vec![0u8; 24];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    Argon2::default()
        .hash_password_into(pass.as_bytes(), &salt, &mut key)
        .context("Could not hash password")?;

    let cipher = XChaCha20Poly1305::new(&key.into());
    let ciphertext = cipher
        .encrypt(
            nonce.as_slice().into(),
            &*serde_json::to_vec(&session).context("could not serialize session")?,
        )
        .map_err(|_| Error::msg("Could not encrypt blob"))?;
    let blob = Blob {
        version: "argon2+chacha20poly1305".to_string(),
        ciphertext,
        salt,
        nonce,
    };

    let user_dir = Path::new(&args().state_dir).join(nick);
    if !user_dir.is_dir() {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&user_dir)
            .context("mkdir of user dir failed")?
    }
    let mut file = fs::OpenOptions::new()
        .mode(0o400)
        .write(true)
        .create_new(true)
        .open(user_dir.join("session"))
        .context("creating user session file failed")?;
    file.write_all(&serde_json::to_vec(&blob).context("could not serialize blob")?)
        .context("Writing to user session file failed")?;
    Ok(())
}

/// Initial "log in": if user exists validate its password,
/// otherwise just let it through iff we allow new users
pub fn login(nick: &str, pass: &str) -> Result<Option<Session>> {
    let session_file = Path::new(&args().state_dir).join(nick).join("session");
    if session_file.is_file() {
        Ok(Some(check_pass(session_file, pass)?))
    } else if args().allow_register {
        Ok(None)
    } else {
        Err(Error::msg(format!("unknown user {}", nick)))
    }
}
