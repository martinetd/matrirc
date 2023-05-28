use anyhow::{Context, Error, Result};
use log::info;
use pwbox::{sodium::Sodium, ErasedPwBox, Eraser, Suite};
use rand_core::OsRng;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::args::args;

// data we want to keep around
#[derive(serde::Serialize, serde::Deserialize)]
struct Session {
    homeserver: String,
    //matrix_session: matrix_sdk::Session;
}

fn check_pass(session_file: PathBuf, pass: &str) -> Result<()> {
    let blob = {
        let content = fs::read(session_file).context("Could not read user session file")?;
        serde_json::from_slice::<ErasedPwBox>(&content)
            .context("Could not deserialize session file content")?
    };
    let mut eraser = Eraser::new();
    eraser.add_suite::<Sodium>();
    let unboxed = eraser
        .restore(&blob)
        .context("Could not parse data on disk")?
        .open(pass.as_bytes())
        .context("Could not decrypt stored session")?;
    let session = serde_json::from_slice::<Session>(&*unboxed)
        .context("Could not deserialize stored session")?;
    info!("Decrypted {}", session.homeserver);
    Ok(())
}

fn create_user(user_dir: PathBuf, pass: &str) -> Result<()> {
    let session = Session {
        homeserver: "hardcoded test".to_string(),
    };
    let pwbox = Sodium::build_box(&mut OsRng).seal(
        pass,
        serde_json::to_vec(&session).context("could not serialize session")?,
    )?;
    let mut eraser = Eraser::new();
    eraser.add_suite::<Sodium>();
    let blob = eraser.erase(&pwbox)?;
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

pub fn login(nick: &str, pass: &str) -> Result<()> {
    let session_file = Path::new(&args().state_dir).join(nick).join("session");
    if session_file.is_file() {
        check_pass(session_file, pass)
    } else if args().allow_register {
        create_user(Path::new(&args().state_dir).join(nick), pass)
    } else {
        Err(Error::msg(format!("unknown user {}", nick)))
    }
}
