#!/usr/bin/env bash
set -euo pipefail

script_path="scripts/check-public-repository-hygiene.sh"
failed=0
reported=0

machine_path_pattern='(/Users/[[:alnum:]_.-]+/|/home/[[:alnum:]_.-]+/|[A-Za-z]:\\Users\\|file:///(Users|home)/)'
# An opening parenthesis after `.local` or `.internal` is a method call, not a hostname.
internal_host_pattern='((^|[^[:alnum:]_.-])([[:alnum:]][[:alnum:]_-]*\.)+(internal|local)([^[:alnum:]_.(-]|$)|https?://(10\.|192\.168\.|172\.(1[6-9]|2[0-9]|3[01])\.))'
credential_pattern='(github_pat_[[:alnum:]_]{20,}|gh[pousr]_[[:alnum:]]{20,}|AKIA[0-9A-Z]{16}|sk-[A-Za-z0-9_-]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----)'
combined_pattern="($machine_path_pattern)|($internal_host_pattern)|($credential_pattern)"
mixed_case_initialism_pattern='(^|[^[:alnum:]_])([C]pu|[M]lx|[U]qa)[[:alnum:]_]*([^[:alnum:]_]|$)'

report_matches() {
  if (( reported == 0 )); then
    printf 'Public repository hygiene violations found:\n' >&2
    reported=1
  fi
  printf '%s\n' "$1" >&2
  failed=1
}

if matches=$(git grep -I -n -E -- "$combined_pattern" -- . ":(exclude)$script_path"); then
  report_matches "$matches"
fi

if matches=$(git grep -I -n -E -- "$mixed_case_initialism_pattern" -- . ":(exclude)$script_path"); then
  report_matches "$matches"
fi

while IFS= read -r -d '' file; do
  matches=""

  [[ -f "$file" ]] || continue
  [[ "$file" == "$script_path" ]] && continue

  if matches=$(LC_ALL=C grep -I -H -n -E -- "$combined_pattern" "$file"); then
    report_matches "$matches"
  fi
  if matches=$(LC_ALL=C grep -I -H -n -E -- "$mixed_case_initialism_pattern" "$file"); then
    report_matches "$matches"
  fi
done < <(git ls-files -z --others --exclude-standard)

if (( failed != 0 )); then
  exit 1
fi

printf 'Public repository hygiene OK\n'
