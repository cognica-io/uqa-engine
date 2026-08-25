//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::env;
use std::net::TcpListener;
use std::process::Command;
use std::thread;

use super::fixture_server::{serve_matrix, MatrixEvent, MatrixObservation};

fn run_driver(image: &str, driver: &str, expects_binary_parameter: bool) {
    let listener = TcpListener::bind(("0.0.0.0", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || serve_matrix(&listener));
    let host = env::var("UQA_PG18_DOCKER_HOST")
        .expect("UQA_PG18_DOCKER_HOST must name this host as reachable from the client container");
    let dsn = format!(
        "postgresql://uqa:matrix-password@{host}:{port}/uqa?sslmode=disable&connect_timeout=10"
    );
    let output = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-e",
            &format!("UQA_PG18_MATRIX_DSN={dsn}"),
            image,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{driver} matrix failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("\"driver\": \"{driver}\""))
            || stdout.contains(&format!("\"driver\":\"{driver}\"")),
        "missing deterministic {driver} evidence: {stdout}"
    );
    let observation = server.join().unwrap();
    assert_matrix_observation(&observation, expects_binary_parameter);
}

fn assert_matrix_observation(observation: &MatrixObservation, expects_binary_parameter: bool) {
    assert!(
        observation.observed(MatrixEvent::PasswordAuthentication),
        "{observation:?}"
    );
    assert_eq!(
        observation.observed(MatrixEvent::BinaryParameter),
        expects_binary_parameter,
        "unexpected binary-parameter behavior: {observation:?}"
    );
    for event in [
        MatrixEvent::BinaryResult,
        MatrixEvent::PreparedReuse,
        MatrixEvent::CopyIn,
        MatrixEvent::CopyOut,
        MatrixEvent::FailedTransaction,
        MatrixEvent::RollbackRecovery,
    ] {
        assert!(
            observation.observed(event),
            "missing {event:?}: {observation:?}"
        );
    }
}

#[test]
#[ignore = "requires tests/parity/pg18/clients/run.sh build and UQA_PG18_DOCKER_HOST"]
fn psycopg_uses_binary_extended_copy_transaction_and_pool_contracts() {
    run_driver("uqa-pg18-client-psycopg:3.3.4", "psycopg", true);
}

#[test]
#[ignore = "requires tests/parity/pg18/clients/run.sh build and UQA_PG18_DOCKER_HOST"]
fn pgx_uses_binary_extended_copy_transaction_and_pool_contracts() {
    run_driver("uqa-pg18-client-pgx:5.10.0", "pgx", true);
}

#[test]
#[ignore = "requires tests/parity/pg18/clients/run.sh build and UQA_PG18_DOCKER_HOST"]
fn node_postgres_uses_extended_copy_transaction_and_pool_contracts() {
    run_driver(
        "uqa-pg18-client-node-postgres:8.23.0",
        "node-postgres",
        false,
    );
}
