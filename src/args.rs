use clap::Parser;
use lazy_static::lazy_static;
use serde::Deserialize;
use std::net::SocketAddr;

#[derive(clap::ValueEnum, Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoJoinOptions {
    None,
    Queries,
    Channels,
    All,
}

impl AutoJoinOptions {
    pub fn join_queries(&self) -> bool {
        [AutoJoinOptions::Queries, AutoJoinOptions::All].contains(self)
    }
    pub fn join_channels(&self) -> bool {
        [AutoJoinOptions::Channels, AutoJoinOptions::All].contains(self)
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short = 'l', long, default_value = "[::1]:6667")]
    pub ircd_listen: SocketAddr,

    #[arg(long, default_value_t = false)]
    pub allow_register: bool,

    #[arg(long, default_value = "/var/lib/matrirc")]
    pub state_dir: String,

    #[arg(long, default_value = None)]
    pub media_dir: Option<String>,

    #[arg(long, default_value = None)]
    pub media_url: Option<String>,

    #[arg(long, value_enum, default_value_t = AutoJoinOptions::None)]
    pub autojoin: AutoJoinOptions,
}

pub fn args() -> &'static Args {
    lazy_static! {
        static ref ARGS: Args = Args::parse();
    }
    &ARGS
}
