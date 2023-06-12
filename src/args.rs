use clap::Parser;
use lazy_static::lazy_static;
use std::net::SocketAddr;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short = 'l', long, default_value = "[::1]:6667")]
    pub ircd_listen: SocketAddr,

    #[arg(long, default_value_t = false)]
    pub allow_register: bool,

    #[arg(long, default_value = "/var/lib/matrirc")]
    pub state_dir: String,
}

pub fn args() -> &'static Args {
    lazy_static! {
        static ref ARGS: Args = Args::parse();
    }
    &ARGS
}
