#!/usr/bin/env bash
set -euo pipefail

df -h /

sudo rm -rf \
  /usr/local/lib/android \
  /usr/share/dotnet \
  /opt/ghc \
  /opt/hostedtoolcache/CodeQL \
  /usr/local/share/boost

docker image prune -af || true

df -h /
