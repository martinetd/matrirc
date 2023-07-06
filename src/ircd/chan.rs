use anyhow::Result;

use crate::ircd::{
    proto::{join, raw_msg},
    IrcClient,
};

pub async fn join_irc_chan(irc: &IrcClient, chan: String, members: Vec<String>) -> Result<()> {
    irc.send(join(None::<String>, &chan)).await?;
    let names_list_header = format!(":matrirc 353 {} = {} :", irc.nick, chan);
    let mut names_list = names_list_header.clone();
    for member in members {
        names_list.push_str(&member);
        if names_list.len() > 400 {
            irc.send(raw_msg(names_list)).await?;
            names_list = names_list_header.clone();
        } else {
            names_list.push(' ');
        }
    }
    if names_list != names_list_header {
        irc.send(raw_msg(names_list)).await?;
    }
    irc.send(raw_msg(format!(":matrirc 366 {} {} :End", irc.nick, chan)))
        .await?;
    Ok(())
}
