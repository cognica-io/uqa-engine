#!/usr/bin/env bash
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#
# Dispatch the complete CI suites once for the exact remote pull-request HEAD.
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
    --json baseRefName,baseRefOid,headRefName,headRefOid,isCrossRepository,state,url \
    --jq '[.state, .isCrossRepository, .baseRefName, .baseRefOid, .headRefName, .headRefOid, .url] | @tsv'
)"
IFS=$'\t' read -r state cross_repository base_ref base_revision head_ref remote_head pr_url <<<"$pr_row"

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

dispatch_workflow() {
  local workflow="$1"
  shift

  local existing_url
  existing_url="$(
    gh run list \
      --workflow "$workflow" \
      --branch "$branch" \
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
    echo "would dispatch $workflow for $branch at $local_head"
    return
  fi

  gh workflow run "$workflow" --ref "$branch" "$@"
}

echo "pre-merge CI target: $pr_url"
echo "head: $local_head"
echo "base: $base_revision"

dispatch_workflow ci.yml -f "base_revision=$base_revision"
dispatch_workflow javascript-bindings.yml
dispatch_workflow python-wheels.yml

if [[ "$dry_run" == 0 ]]; then
  echo "pre-merge CI dispatched; monitor with: gh run list --branch $branch"
  echo "any later push changes the required HEAD and needs one new pre-merge run"
fi
