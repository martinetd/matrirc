use clap::Parser;
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

static mut ARGS: Option<Args> = None;

pub fn parse() {
    let args = Args::parse();
    // parse is only called once at startup
    unsafe {
        ARGS = Some(args);
    }
}

pub fn args() -> &'static Args {
    unsafe {
        // args is never modified after init
        if let Some(args) = ARGS.as_ref() {
            return args;
        }
    }
    panic!("args was not initialized?!");
}
