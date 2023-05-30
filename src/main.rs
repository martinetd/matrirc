mod args;
mod ircd;
mod matrix;
mod state;

#[tokio::main]
async fn main() {
    env_logger::init();
    args::parse();

    let ircd = ircd::listen().await;

    ircd.await.unwrap();
}
