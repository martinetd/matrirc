use anyhow::{Context, Error, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use serde;
use std::fs;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use crate::args::args;

#[derive(serde::Serialize, serde::Deserialize)]
struct Hash {
    algo: String, // only support "argon2"
    hash: String, // contains salt and argon2 params
}

fn check_pass(creds_file: PathBuf, pass: &str) -> Result<()> {
    let disk_hash = {
        let content = fs::read_to_string(creds_file).context("Could not read user creds file")?;
        toml::from_str::<Hash>(&content).context("Could not deserialize creds file content")?
    };
    if disk_hash.algo != "argon2" {
        return Err(Error::msg("Had bad hash on disk"));
    }
    let parsed_hash =
        PasswordHash::new(&disk_hash.hash).context("Could not parse on-disk password")?;
    Argon2::default()
        .verify_password(pass.as_bytes(), &parsed_hash)
        .context("User password mismatch")?;
    Ok(())
}

fn create_user(user_dir: PathBuf, pass: &str) -> Result<()> {
    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(pass.as_bytes(), &salt)
        .context("hashing password failed")?;
    let hash = Hash {
        algo: "argon2".to_string(),
        hash: password_hash.serialize().to_string(),
    };
    if !user_dir.is_dir() {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&user_dir)
            .context("mkdir of user dir failed")?
    }
    let mut file =
        fs::File::create(user_dir.join("creds.toml")).context("creating user creds file failed")?;
    file.write_all(
        toml::to_string(&hash)
            .context("could not serialize hash")?
            .as_bytes(),
    )
    .context("Writing to user creds file failed")?;
    Ok(())
}

pub fn login(nick: &str, pass: &str) -> Result<()> {
    let creds_file = Path::new(&args().state_dir).join(nick).join("creds.toml");
    if creds_file.is_file() {
        check_pass(creds_file, pass)
    } else if args().allow_register {
        create_user(Path::new(&args().state_dir).join(nick), pass)
    } else {
        Err(Error::msg(format!("unknown user {}", nick)))
    }
}
