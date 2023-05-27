#!/bin/sh

command -v rustfmt >/dev/null || error "Install rustfmt first"

src="$(git rev-parse --show-toplevel)/scripts/commit_hook"
dest="$(git rev-parse --git-dir)/hooks/pre-commit"

cp "$src" "$dest" || exit
chmod +x "$dest"
echo "Installed $dest"
