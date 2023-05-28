use irc::client::prelude::{Command, Message};

pub fn raw_msg(msg: String) -> Message {
    Message {
        tags: None,
        prefix: None,
        command: Command::Raw(msg, vec![]),
    }
}
