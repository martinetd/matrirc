use anyhow::{Context, Result};
use futures::SinkExt;
use irc::{
    client::prelude::{Command, Message},
    proto::error::ProtocolError,
};

fn raw_msg(msg: String) -> Message {
    Message {
        tags: None,
        prefix: None,
        command: Command::Raw(msg, vec![]),
    }
}

pub async fn send_raw_msg<'a, S>(stream: &mut S, msg: String) -> Result<()>
where
    S: SinkExt<Message, Error = ProtocolError> + std::marker::Unpin,
{
    stream.send(raw_msg(msg)).await.context("send_raw_msg")
}

//fn privmsg(prefix: String, target: String, message: String) {
//pub fn privmsg
