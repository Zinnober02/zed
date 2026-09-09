#!/bin/sh
# Point git at the tracked hook directory so hooks travel with the repository.
set -e
root=$(git rev-parse --show-toplevel)
git -C "$root" config core.hooksPath .githooks
chmod +x "$root/.githooks/"* 2>/dev/null || true
echo "git hooks installed from $root/.githooks"
