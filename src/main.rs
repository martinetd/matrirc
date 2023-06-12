mod args;
mod ircd;
mod matrirc;
mod matrix;
mod state;

#[tokio::main]
async fn main() {
    env_logger::init();
    // ensure args parse early
    let _ = args::args();

    let ircd = ircd::listen().await;

    ircd.await.unwrap();
}
