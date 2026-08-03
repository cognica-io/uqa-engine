#!/usr/bin/env bash
set -euo pipefail

if perl -MIPC::Cmd -e 1 2>/dev/null; then
  exit 0
fi

if command -v dnf >/dev/null 2>&1; then
  dnf install -y perl-IPC-Cmd
elif command -v yum >/dev/null 2>&1; then
  yum install -y perl-IPC-Cmd
else
  echo "No supported package manager is available to install Perl IPC::Cmd" >&2
  exit 1
fi

perl -MIPC::Cmd -e 1
