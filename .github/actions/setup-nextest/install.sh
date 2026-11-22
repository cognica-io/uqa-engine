#!/usr/bin/env bash
set -euo pipefail

url=$1
checksum=$2
cache="$RUNNER_TOOL_CACHE/cargo-nextest"
mkdir -p "$cache"
staging=$(mktemp -d "$cache/.install.XXXXXX")
archive="$staging/nextest.tar.gz"
cached="$cache/$checksum.tar.gz"
trap 'rm -f "$archive"; rmdir "$staging"' EXIT

verified() {
  printf '%s  %s\n' "$checksum" "$archive" | shasum -a 256 --check --status
}

download() {
  local attempt status remaining deadline=$((SECONDS + 540))
  : > "$archive"
  for attempt in 1 2 3; do
    remaining=$((deadline - SECONDS))
    if ((remaining <= 0)); then
      echo "cargo-nextest download exceeded the nine-minute transfer budget" >&2
      return 28
    fi
    status=0
    curl --fail --silent --show-error --location --continue-at - \
      --connect-timeout 30 --speed-limit 1024 --speed-time 60 --max-time "$remaining" \
      --output "$archive" "$url" || status=$?
    if verified; then return 0; fi
    if [[ "$status" == 0 ]]; then
      echo "Downloaded cargo-nextest archive failed SHA-256 verification" >&2
      return 1
    fi
    if [[ "$attempt" == 3 ]]; then return "$status"; fi
    # A server that does not support the requested range needs a fresh transfer.
    case "$status" in 33|36) : > "$archive" ;; esac
    sleep "$attempt"
  done
}

publish=false
if [[ -f "$cached" ]] && cp "$cached" "$archive" && verified; then
  echo "Using the verified cargo-nextest archive from the runner tool cache"
else
  download
  publish=true
fi

directory=$(mktemp -d "$RUNNER_TEMP/nextest.XXXXXX")
tar -xzf "$archive" -C "$directory" cargo-nextest
if [[ "$publish" == true ]]; then
  # Readers see only a complete archive, even when jobs share the tool cache.
  mv -f "$archive" "$cached"
fi
printf '%s\n' "$directory" >> "$GITHUB_PATH"
