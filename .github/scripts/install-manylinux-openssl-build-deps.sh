#!/usr/bin/env bash
set -euo pipefail

if perl -MIPC::Cmd -MTime::Piece -e 1 2>/dev/null; then
  exit 0
fi

if command -v dnf >/dev/null 2>&1; then
  dnf install -y perl-core
elif command -v yum >/dev/null 2>&1; then
  yum install -y perl-core
else
  echo "No supported package manager is available to install Perl core modules" >&2
  exit 1
fi

perl -MIPC::Cmd -MTime::Piece -e 1
