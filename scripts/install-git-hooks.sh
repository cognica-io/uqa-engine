#!/bin/sh
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.
set -eu

root=$(git rev-parse --show-toplevel)
current=$(git config --get core.hooksPath || true)
if [ -n "$current" ] && [ "$current" != .githooks ]; then
    echo "Existing core.hooksPath is $current; preserve its hooks before changing the path." >&2
    exit 1
fi
hooks=$(git rev-parse --git-path hooks)
if [ -z "$current" ] && [ -f "$hooks/pre-commit" ]; then
    echo "An existing pre-commit hook is installed at $hooks/pre-commit; preserve it before installing." >&2
    exit 1
fi
test -x "$root/.githooks/pre-commit"
git config --local core.hooksPath .githooks
echo "Installed the staged crate-dependency check for this repository."
