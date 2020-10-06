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
- either save matrix state along to keep from one connect to
the next or cleanup properly when irc client disconnects
   -> lock doesn't work well, probably want to keep state
around just not syncing. will still need lock to not have
parallel syncs....
- make a synthetic channel or user for e.g. device verifications,
invite notices etc
- add a list of ignored channels user doesn't care about
- handle m.reaction
Message(Custom(SyncMessageEvent { content: CustomEventContent { event_type: "m.reaction", json: Object({"m.relates_to": Object({"event_id": String("xxx"), "key": String("😄"), "rel_type": String("m.annotation")})}) }, event_id: EventId { full_id: "xxx", colon_idx: None }, sender: UserId { full_id: "@xxx:matrix.xxx", colon_idx: 7, is_historical: false }, origin_server_ts: SystemTime { tv_sec: 1601823096, tv_nsec: 97000000 }, unsigned: Unsigned { age: Some(xxx), transaction_id: None } }))
(look for relates to object by event id :/)
- somehow type irc nick/mask/chan and add conversion operators from matrix
