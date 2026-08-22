#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Dispatch change-aware pre-merge suites once for the exact remote pull-request HEAD.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

force=0
dry_run=0

usage() {
  echo "usage: bash scripts/run-premerge-ci.sh [--dry-run] [--force]" >&2
}

while (($#)); do
  case "$1" in
    --dry-run)
      dry_run=1
      ;;
    --force)
      force=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
  shift
done

if ! command -v gh >/dev/null 2>&1; then
  echo "gh is required to dispatch pre-merge CI" >&2
  exit 1
fi

branch="$(git symbolic-ref --quiet --short HEAD)" || {
  echo "pre-merge CI requires a checked-out feature or fix branch" >&2
  exit 1
}

if [[ "$branch" == "main" ]]; then
  echo "pre-merge CI must run from the pull-request branch, not main" >&2
  exit 1
fi

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "commit tracked changes before dispatching pre-merge CI" >&2
  exit 1
fi

pr_row="$(
  gh pr view "$branch" \
    --json baseRefName,baseRefOid,headRefName,headRefOid,isCrossRepository,number,state,url \
    --jq '[.state, .isCrossRepository, .number, .baseRefName, .baseRefOid, .headRefName, .headRefOid, .url] | @tsv'
)"
IFS=$'\t' read -r state cross_repository pr_number base_ref base_revision head_ref remote_head pr_url <<<"$pr_row"

if [[ "$state" != "OPEN" ]]; then
  echo "pull request for $branch is not open" >&2
  exit 1
fi

if [[ "$cross_repository" != "false" ]]; then
  echo "pre-merge CI requires a branch in the origin repository" >&2
  exit 1
fi

if [[ "$base_ref" != "main" ]]; then
  echo "pull request base must be main, found $base_ref" >&2
  exit 1
fi

if [[ "$head_ref" != "$branch" ]]; then
  echo "pull request head $head_ref does not match checked-out branch $branch" >&2
  exit 1
fi

local_head="$(git rev-parse HEAD)"
if [[ "$local_head" != "$remote_head" ]]; then
  echo "local HEAD $local_head does not match remote pull-request HEAD $remote_head" >&2
  echo "push the final commit before dispatching pre-merge CI" >&2
  exit 1
fi

changed_files="$(git diff --name-only "$base_revision" "$local_head")"
run_rust=false
run_javascript=false
run_python=false

while IFS= read -r path; do
  [[ -n "$path" ]] || continue

  case "$path" in
    Cargo.toml|Cargo.lock)
      run_rust=true
      run_javascript=true
      run_python=true
      ;;
    deny.toml|rust-toolchain*|.cargo/*|crates/*.rs|crates/*/Cargo.toml|\
      crates/*/build.rs|crates/uqa-pg-query/libpg_query/*|examples/rust/*|\
      benchmarks/*|tests/*.rs|tests/parity/*|docs/manual/*|\
      .github/scripts/*|.github/workflows/ci.yml)
      run_rust=true
      ;;
  esac

  case "$path" in
    crates/uqa-node/*|crates/uqa-wasm/*|tests/node/*|tests/wasm/*|\
      examples/node/*|examples/browser/*|scripts/build-wasm.sh|\
      scripts/npm-release.py|.github/workflows/javascript-bindings.yml)
      run_javascript=true
      ;;
  esac

  case "$path" in
    pyproject.toml|python/*|crates/uqa-python/*|tests/python/*|\
      examples/python/*|.github/workflows/python-wheels.yml)
      run_python=true
      ;;
  esac
done <<<"$changed_files"

temporary_tag=""

cleanup_temporary_tag() {
  local status=$?
  trap - EXIT

  if [[ -n "$temporary_tag" ]]; then
    if git push origin ":refs/tags/$temporary_tag"; then
      echo "removed temporary dispatch tag $temporary_tag"
    else
      echo "failed to remove temporary dispatch tag $temporary_tag" >&2
      status=1
    fi
  fi

  exit "$status"
}

trap cleanup_temporary_tag EXIT

ensure_dispatch_ref() {
  if [[ -n "$temporary_tag" ]]; then
    return
  fi

  local candidate timestamp
  timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
  candidate="uqa-premerge/pr-${pr_number}-${local_head:0:12}-${timestamp}-$$"
  git push origin "${local_head}:refs/tags/${candidate}"
  temporary_tag="$candidate"
  echo "created temporary dispatch tag $temporary_tag at $local_head"
}

dispatch_workflow() {
  local workflow="$1"
  shift

  local existing_url
  existing_url="$(
    gh run list \
      --workflow "$workflow" \
      --commit "$local_head" \
      --limit 1 \
      --json url \
      --jq '.[0].url // empty'
  )"

  if [[ -n "$existing_url" && "$force" == 0 ]]; then
    echo "$workflow already has a run for $local_head: $existing_url"
    return
  fi

  if [[ "$dry_run" == 1 ]]; then
    echo "would dispatch $workflow through a temporary tag at $local_head"
    return
  fi

  ensure_dispatch_ref
  gh workflow run "$workflow" --ref "$temporary_tag" "$@"
}

echo "pre-merge CI target: $pr_url"
echo "head: $local_head"
echo "base: $base_revision"
echo "Rust suite: $run_rust"
echo "JavaScript/WebAssembly suite: $run_javascript"
echo "Python suite: $run_python"

dispatch_workflow ci.yml -f "run_rust=$run_rust"
if [[ "$run_javascript" == true ]]; then
  dispatch_workflow javascript-bindings.yml
fi
if [[ "$run_python" == true ]]; then
  dispatch_workflow python-wheels.yml
fi

if [[ "$dry_run" == 0 ]]; then
  echo "pre-merge CI dispatched; monitor with: gh run list --commit $local_head"
  echo "any later push changes the required HEAD and needs one new pre-merge run"
fi
