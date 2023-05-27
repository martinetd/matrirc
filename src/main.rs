extern crate env_logger;

mod args;
mod ircd;

fn main() {
    env_logger::init();
    args::parse();

    ircd::listen();
}
