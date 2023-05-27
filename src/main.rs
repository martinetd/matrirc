mod args;
mod ircd;

#[tokio::main]
async fn main() {
    env_logger::init();
    args::parse();

    let ircd = ircd::listen().await;

    ircd.await.unwrap();
}
