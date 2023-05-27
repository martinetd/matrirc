#[macro_use]
extern crate log;

use crate::args::args;

pub fn listen() {
    let args = args();

    info!("listening to {}", args.ircd_listen)
}
