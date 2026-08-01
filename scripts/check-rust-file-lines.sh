#!/usr/bin/env bash
set -euo pipefail

line_limit="${UQA_RUST_FILE_LINE_LIMIT:-1500}"
largest_file=""
largest_lines=0
failed=0

while IFS= read -r -d '' file; do
  lines=$(wc -l < "$file")
  if (( lines > largest_lines )); then
    largest_file="$file"
    largest_lines=$lines
  fi
  if (( lines > line_limit )); then
    printf 'Rust file exceeds %d lines: %s (%d)\n' "$line_limit" "$file" "$lines" >&2
    failed=1
  fi
done < <(
  find . \
    \( -path './.git' -o -path './target' \) -prune -o \
    -type f -name '*.rs' -print0
)

if (( failed != 0 )); then
  exit 1
fi

printf 'Rust file line limit OK (max %d): %s (%d)\n' \
  "$line_limit" "$largest_file" "$largest_lines"
