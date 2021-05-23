# matrirc

simple ircd bridging to matrix

# Project

This has mostly been a toy project to learn rust.

If PRs are sent I will likely look at them and merge, but expect the project
to be otherwise unmaintained in the long run and do not use unless you're
ready to get your hands dirty.

# Usage

Start one instance per user (might be subject to change)

Config file in ~/.config/matrirc/config as en env file with keys:
- `HOMESERVER`: url of the homeserver
- `IRC_PASSWORD`: optional required PASS command to give.
Note that `:` in the irc-provided password will be used as a separator
to read matrix store's pickle password from if given.
- `ACCESS_TOKEN`, `USER_ID` and `DEVICE_ID` : as given the first time
you login (first login without these declared will prompt for user
and password)

# Todo

- see XXX in the code
- make a synthetic channel or user for e.g. device verifications,
invite notices etc
- add a list of ignored channels user doesn't care about...
Or automatic if no activity for > x months?
- ideally handle direct chats in query, what if duplicate?
- cleanup/do not join channels when it gets renamed...
- lookup message by eventId if not in LRU for react/redact:
https://github.com/matrix-org/matrix-rust-sdk/issues/242
- somehow type irc nick/mask/chan and add conversion operators from matrix
- look into irssi errors:
19:20:35 -!- Irssi: critical nicklist_set_host: assertion 'host != NULL' failed
Probably some mask malformed?

