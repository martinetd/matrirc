# matrirc

simple ircd bridging to matrix

# Features

- e2e encryption
- client verification
- can accept encrypted files to local directory (`--media-dir`) and give links if configured (`--media-url`, prefix up to file name).
You'll need to configure cleanup yourself at this point.

# Usage

- Run server with `--allow-register`, connect from an irc client with a password set
- Follow prompt to login to your account
- Once logged in, we remember you from nick/password: you can reconnect without `--allow-register` and get your session back

# TODO

Things known not to work, planned:
 - channels (currently emulated in queries), including join/parts
 - joning all rooms on reconnect (can only respond if there was recent activity right now)
 - reactions
 - redactions
 - room invitations
 - notification on topic/icon change

 Not planned short term, but would accept PR:
  - initiate joining room from irc (add metacommand through 'matrirc' queries, like verification)
  - mentions (look for @nick in messages -> search nick in room members -> translate to real userId for highlight)
  - mentions, other way around (translate @userId to @nick)
