#!/bin/sh
# Unified Query Algebra
# Copyright (c) 2023-2026 Cognica, Inc.
set -eu

# Release packaging replaces this with the version of the built source.
version='@UQA_VERSION@'
bin_dir="${HOME}/.local/bin"
archive_dir=''
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bin-dir|--archive-dir)
            [ "$#" -ge 2 ] || { echo "Missing value for $1" >&2; exit 1; }
            case "$1" in
                --bin-dir) bin_dir=$2 ;;
                --archive-dir) archive_dir=$2 ;;
            esac
            shift 2 ;;
        --help)
            echo 'Usage: sh install-usql.sh [--bin-dir DIR] [--archive-dir DIR]'
            exit 0 ;;
        *) echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done
case "$version" in @*) echo 'Use the installer attached to a release.' >&2; exit 1 ;; esac
[ -n "$bin_dir" ] || { echo 'Installation directory is empty.' >&2; exit 1; }
[ ! -d "$bin_dir/usql" ] || { echo 'The usql destination is a directory.' >&2; exit 1; }
case "$(uname -s)" in
    Linux) platform=unknown-linux-gnu ;;
    Darwin) platform=apple-darwin ;;
    *) echo 'Supported systems: Linux and macOS. Use install-usql.ps1 on Windows.' >&2; exit 1 ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) echo 'Supported architectures: x86_64 and ARM64.' >&2; exit 1 ;;
esac
asset="uqa-cli-${version}-${arch}-${platform}.tgz"
temp_dir=$(mktemp -d)
staged=''
cleanup() {
    rm -rf "$temp_dir"
    [ -z "$staged" ] || rm -f "$staged"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
if [ -n "$archive_dir" ]; then
    cp "$archive_dir/$asset" "$temp_dir/$asset"
    cp "$archive_dir/$asset.sha256" "$temp_dir/$asset.sha256"
else
    base="https://github.com/cognica-io/uqa-engine/releases/download/v${version}"
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' "$base/$asset" -o "$temp_dir/$asset"
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' "$base/$asset.sha256" -o "$temp_dir/$asset.sha256"
fi
expected=$(awk -v name="$asset" 'NF == 2 && $2 == name && length($1) == 64 && $1 !~ /[^0-9a-f]/ { print $1 }' "$temp_dir/$asset.sha256")
[ "${#expected}" -eq 64 ] || { echo 'Invalid SHA-256 checksum file.' >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temp_dir/$asset" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$temp_dir/$asset" | cut -d ' ' -f 1)
fi
[ "$actual" = "$expected" ] || { echo 'SHA-256 checksum mismatch.' >&2; exit 1; }
tar -xzf "$temp_dir/$asset" -C "$temp_dir" usql
[ -f "$temp_dir/usql" ] && [ ! -L "$temp_dir/usql" ] || { echo 'Archive has no regular usql executable.' >&2; exit 1; }
mkdir -p "$bin_dir"
staged=$(mktemp "$bin_dir/.usql.XXXXXX")
cp "$temp_dir/usql" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$bin_dir/usql"
staged=''
printf 'Installed usql %s to %s/usql\n' "$version" "$bin_dir"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) printf 'Add %s to PATH to run usql.\n' "$bin_dir" ;;
esac
