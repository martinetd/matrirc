use matrix_sdk::Client;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::ircd::client::IrcClient;

/// client state struct
#[derive(Clone, Debug)]
pub struct Matrirc {
    pub matrix: Client,
    pub irc: IrcClient,
    pub running: Arc<RwLock<bool>>,
}

impl Matrirc {
    pub fn new(matrix: Client, irc: IrcClient) -> Matrirc {
        Matrirc {
            matrix,
            irc,
            running: Arc::new(RwLock::new(true)),
        }
    }
}
