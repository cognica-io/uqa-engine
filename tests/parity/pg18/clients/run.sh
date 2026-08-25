#!/usr/bin/env bash

set -euo pipefail

client_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$client_root/../../../.." && pwd)"
oracle_container="${UQA_PG18_WIRE_CONTAINER:-pg-parity}"
docker_host="${UQA_PG18_DOCKER_HOST:-host.docker.internal}"
oracle_port="${UQA_PG18_ORACLE_PORT:-15432}"
target_dir="${CARGO_TARGET_DIR:-/private/tmp/uqa-target-protocol-client}"
evidence_dir="$(mktemp -d "${TMPDIR:-/tmp}/uqa-pg18-clients.XXXXXX")"
trap 'rm -rf "$evidence_dir"' EXIT

docker build -t uqa-pg18-client-psycopg:3.3.4 "$client_root/psycopg"
docker build -t uqa-pg18-client-pgx:5.10.0 "$client_root/pgx"
docker build -t uqa-pg18-client-node-postgres:8.23.0 "$client_root/node-postgres"

docker exec -i "$oracle_container" psql -X -U postgres -d postgres < "$client_root/oracle_setup.sql"
oracle_dsn="postgresql://uqa_matrix:uqa-matrix-password@${docker_host}:${oracle_port}/postgres?sslmode=disable&connect_timeout=10"

run_oracle() {
    local driver="$1"
    local image="$2"
    local output="$evidence_dir/${driver}.jsonl"
    docker run --rm -e "UQA_PG18_MATRIX_DSN=$oracle_dsn" "$image" > "$output"
    python3 "$client_root/check_evidence.py" "$driver" "$output"
}

run_oracle psycopg uqa-pg18-client-psycopg:3.3.4
run_oracle pgx uqa-pg18-client-pgx:5.10.0
run_oracle node-postgres uqa-pg18-client-node-postgres:8.23.0

cd "$repo_root"
UQA_PG18_DOCKER_HOST="$docker_host" \
    CARGO_TARGET_DIR="$target_dir" \
    CARGO_PROFILE_TEST_DEBUG=0 \
    CARGO_PROFILE_DEV_DEBUG=0 \
    CARGO_INCREMENTAL=0 \
    cargo test -p uqa-pg-wire --test protocol client_matrix:: -- --ignored --test-threads=1

UQA_PG18_DOCKER_HOST="$docker_host" \
    UQA_PG18_WIRE_CONTAINER="$oracle_container" \
    CARGO_TARGET_DIR="$target_dir" \
    CARGO_PROFILE_TEST_DEBUG=0 \
    CARGO_PROFILE_DEV_DEBUG=0 \
    CARGO_INCREMENTAL=0 \
    cargo test -p uqa-pg-wire --test protocol libpq_interop:: -- --ignored --test-threads=1
