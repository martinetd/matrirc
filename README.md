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
- `IRC_PASSWORD`: optional required PASS command to give
- `ACCESS_TOKEN`, `USER_ID` and `DEVICE_ID` : as given the first time
you login (first login without these declared will prompt for user
and password)

# Todo

- make ircd bind url configurable
- see XXX in the code
- either save matrix state along to keep from one connect to
the next or cleanup properly when irc client disconnects
- make a synthetic channel or user
- add a list of ignored channels user doesn't care about
- handle m.reaction, m.emote...
