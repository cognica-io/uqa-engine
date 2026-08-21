#!/usr/bin/env bash
set -euo pipefail

expected_header=$'//\n// Unified Query Algebra\n//\n// Copyright (c) 2023-2026 Cognica, Inc.\n//'
checked=0
failed=0

while IFS= read -r -d '' file; do
  [[ -f "$file" ]] || continue
  [[ "$file" == crates/uqa-pg-query/* ]] && continue
  first_five=$(sed -n '1,5p' "$file")
  checked=$((checked + 1))
  if [[ "$first_five" != "$expected_header" ]]; then
    printf 'Rust file is missing the standard header: %s\n' "$file" >&2
    failed=1
  fi
done < <(git ls-files -z --cached --others --exclude-standard -- '*.rs')

if (( failed != 0 )); then
  exit 1
fi

printf 'Rust file headers OK: %d files\n' "$checked"
