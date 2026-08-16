//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18 libpq interoperability tests.
//!
//! These tests use `psql` only as a thin libpq driver. The test server is built
//! directly from `uqa-pg-wire` codecs, so successful connections exercise the
//! crate's startup decoder, negotiation response, authentication ordering,
//! backend-key encoding, query decoder, and cancellation-request decoder.

use std::env;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use uqa_pg_wire::backend::{encode_gssenc_response, encode_ssl_response, sqlstate, TYPE_INT4};
use uqa_pg_wire::{
    decode_frontend, decode_startup, encode_all_for_protocol, Authentication, BackendKeyData,
    BackendMessage, CancelKey, DescribeTarget, ErrorOrNotice, FieldDescription, FormatCode,
    FrontendMessage, GSSEncResponse, ProtocolVersion, SSLResponse, StartupFrame, StartupMessage,
    TransactionStatus,
};

const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const BACKEND_PROCESS_ID: i32 = 73_218;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreStartupRequest {
    Gss,
    Ssl,
}

#[derive(Debug)]
struct ConnectionObservation {
    startup: StartupMessage,
    negotiated: ProtocolVersion,
    pre_startup: Vec<PreStartupRequest>,
    query: String,
}

#[derive(Debug)]
struct CancellationObservation {
    connection: ConnectionObservation,
    process_id: i32,
    secret_key: CancelKey,
    cancel_pre_startup: Vec<PreStartupRequest>,
}

#[derive(Debug)]
struct ExtendedQueryObservation {
    startup: StartupMessage,
    negotiated: ProtocolVersion,
    query: String,
    parameter: Vec<u8>,
}

fn container_name() -> String {
    env::var("UQA_PG18_WIRE_CONTAINER").unwrap_or_else(|_| "pg-parity".to_owned())
}

fn docker_host() -> String {
    env::var("UQA_PG18_DOCKER_HOST").expect(
        "UQA_PG18_DOCKER_HOST must name the test server host as reachable from the PostgreSQL 18 client container",
    )
}

fn assert_postgresql_18_libpq() {
    let output = Command::new("docker")
        .args(["exec", &container_name(), "psql", "--version"])
        .output()
        .expect("run PostgreSQL 18 psql in Docker");
    assert!(
        output.status.success(),
        "PostgreSQL 18 client probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        version.contains("psql (PostgreSQL) 18."),
        "expected PostgreSQL 18 libpq client, got {version:?}"
    );
}

fn bind_test_listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind(("0.0.0.0", 0)).expect("bind live libpq test server");
    let port = listener.local_addr().expect("test server address").port();
    (listener, port)
}

fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    let deadline = Instant::now() + ACCEPT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("set accepted client socket blocking");
                stream
                    .set_read_timeout(Some(IO_TIMEOUT))
                    .expect("set client read timeout");
                stream
                    .set_write_timeout(Some(IO_TIMEOUT))
                    .expect("set client write timeout");
                return stream;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for PostgreSQL 18 libpq connection"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept PostgreSQL 18 libpq connection: {error}"),
        }
    }
}

fn read_startup_frame(stream: &mut TcpStream) -> StartupFrame {
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .expect("read startup packet length");
    let length = usize::try_from(u32::from_be_bytes(length)).expect("startup length fits usize");
    assert!(length >= 8, "startup packet is too short: {length}");
    let mut packet = vec![0_u8; length];
    packet[..4].copy_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
    stream
        .read_exact(&mut packet[4..])
        .expect("read startup packet body");
    let Some((frame, consumed)) = decode_startup(&packet).expect("decode live startup packet")
    else {
        panic!("complete startup packet decoded as incomplete");
    };
    assert_eq!(consumed, packet.len());
    frame
}

fn read_startup_after_rejecting_encryption(
    stream: &mut TcpStream,
) -> (StartupFrame, Vec<PreStartupRequest>) {
    let mut requests = Vec::new();
    loop {
        match read_startup_frame(stream) {
            StartupFrame::GSSEncRequest => {
                requests.push(PreStartupRequest::Gss);
                stream
                    .write_all(&encode_gssenc_response(GSSEncResponse::Reject))
                    .expect("reject GSS encryption");
            }
            StartupFrame::SSLRequest => {
                requests.push(PreStartupRequest::Ssl);
                stream
                    .write_all(&encode_ssl_response(SSLResponse::Reject))
                    .expect("reject SSL encryption");
            }
            frame => return (frame, requests),
        }
    }
}

fn read_frontend_message(stream: &mut TcpStream) -> FrontendMessage {
    let mut header = [0_u8; 5];
    stream
        .read_exact(&mut header)
        .expect("read frontend message header");
    let body_length = usize::try_from(u32::from_be_bytes(header[1..5].try_into().unwrap()))
        .expect("frontend message length fits usize");
    assert!(
        body_length >= 4,
        "frontend message body length is too short: {body_length}"
    );
    let total_length = body_length + 1;
    let mut packet = vec![0_u8; total_length];
    packet[..5].copy_from_slice(&header);
    stream
        .read_exact(&mut packet[5..])
        .expect("read frontend message body");
    let Some((message, consumed)) = decode_frontend(&packet).expect("decode live frontend message")
    else {
        panic!("complete frontend message decoded as incomplete");
    };
    assert_eq!(consumed, packet.len());
    message
}

fn cancellation_key(version: ProtocolVersion) -> CancelKey {
    let length = if version >= ProtocolVersion::V3_2 {
        256
    } else {
        4
    };
    let bytes = (0..length)
        .map(|index| u8::try_from(index % 251).unwrap())
        .collect::<Vec<_>>();
    CancelKey::new(bytes).expect("valid protocol-specific cancellation key")
}

fn authenticate(
    stream: &mut TcpStream,
    startup: &StartupMessage,
    newest_supported: ProtocolVersion,
) -> (ProtocolVersion, CancelKey) {
    let negotiation = startup
        .negotiate_with_max(newest_supported, &[])
        .expect("negotiate live startup packet");
    let secret_key = cancellation_key(negotiation.negotiated_version);
    let mut messages = Vec::new();
    if let Some(response) = negotiation.response() {
        messages.push(response);
    }
    messages.extend([
        BackendMessage::Authentication(Authentication::Ok),
        BackendMessage::ParameterStatus {
            name: "server_version".to_owned(),
            value: "18.4".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "server_encoding".to_owned(),
            value: "UTF8".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "client_encoding".to_owned(),
            value: "UTF8".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "standard_conforming_strings".to_owned(),
            value: "on".to_owned(),
        },
        BackendMessage::BackendKeyData(BackendKeyData {
            process_id: BACKEND_PROCESS_ID,
            secret_key: secret_key.clone(),
        }),
        BackendMessage::ReadyForQuery(TransactionStatus::Idle),
    ]);
    let bytes = encode_all_for_protocol(&messages, negotiation.negotiated_version)
        .expect("encode live authentication sequence");
    stream
        .write_all(&bytes)
        .expect("write live authentication sequence");
    (negotiation.negotiated_version, secret_key)
}

fn accept_authenticated(
    listener: &TcpListener,
    newest_supported: ProtocolVersion,
) -> (
    TcpStream,
    StartupMessage,
    ProtocolVersion,
    CancelKey,
    Vec<PreStartupRequest>,
) {
    let mut stream = accept_with_timeout(listener);
    let (frame, pre_startup) = read_startup_after_rejecting_encryption(&mut stream);
    let StartupFrame::Startup(startup) = frame else {
        panic!("expected StartupMessage after encryption negotiation, got {frame:?}");
    };
    let (negotiated, secret_key) = authenticate(&mut stream, &startup, newest_supported);
    (stream, startup, negotiated, secret_key, pre_startup)
}

fn write_select_one(stream: &mut TcpStream, version: ProtocolVersion) {
    let bytes = encode_all_for_protocol(
        &[
            BackendMessage::RowDescription(vec![FieldDescription::text("?column?", TYPE_INT4, 4)]),
            BackendMessage::DataRow(vec![Some(b"1".to_vec())]),
            BackendMessage::CommandComplete("SELECT 1".to_owned()),
            BackendMessage::ReadyForQuery(TransactionStatus::Idle),
        ],
        version,
    )
    .expect("encode SELECT 1 response");
    stream.write_all(&bytes).expect("write SELECT 1 response");
}

fn write_backend_messages(
    stream: &mut TcpStream,
    version: ProtocolVersion,
    messages: &[BackendMessage],
) {
    let bytes = encode_all_for_protocol(messages, version).expect("encode backend messages");
    stream.write_all(&bytes).expect("write backend messages");
}

fn serve_select_one(
    listener: &TcpListener,
    newest_supported: ProtocolVersion,
) -> ConnectionObservation {
    let (mut stream, startup, negotiated, _, pre_startup) =
        accept_authenticated(listener, newest_supported);
    let FrontendMessage::Query(query) = read_frontend_message(&mut stream) else {
        panic!("psql did not use the simple query protocol");
    };
    assert_eq!(query.trim().trim_end_matches(';'), "SELECT 1");
    write_select_one(&mut stream, negotiated);
    assert_eq!(
        read_frontend_message(&mut stream),
        FrontendMessage::Terminate
    );
    ConnectionObservation {
        startup,
        negotiated,
        pre_startup,
        query,
    }
}

fn psql_command(port: u16, max_protocol_version: &str, encryption: bool) -> Command {
    let mut command = Command::new("docker");
    let mut conninfo = format!(
        "host={} port={port} user=uqa dbname=uqa connect_timeout=5 max_protocol_version={max_protocol_version}",
        docker_host()
    );
    if encryption {
        conninfo.push_str(" gssencmode=disable sslmode=prefer sslnegotiation=postgres");
    } else {
        conninfo.push_str(" gssencmode=disable sslmode=disable");
    }
    command.args([
        "exec",
        &container_name(),
        "psql",
        "-X",
        "-v",
        "ON_ERROR_STOP=1",
        &conninfo,
        "-c",
        "\\conninfo",
        "-Atq",
        "-c",
        "SELECT 1",
    ]);
    command
}

fn run_psql(port: u16, max_protocol_version: &str, encryption: bool) -> Output {
    psql_command(port, max_protocol_version, encryption)
        .output()
        .expect("run PostgreSQL 18 psql")
}

fn assert_psql_success(output: &Output, expected_protocol: ProtocolVersion) {
    assert!(
        output.status.success(),
        "psql failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Protocol Version"),
        "missing \\conninfo: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "{}.{}",
            expected_protocol.major, expected_protocol.minor
        )),
        "expected protocol {expected_protocol:?} in \\conninfo: {stdout}"
    );
    assert!(stdout.lines().any(|line| line.trim() == "1"), "{stdout}");
}

#[test]
#[ignore = "requires UQA_PG18_DOCKER_HOST and UQA_PG18_WIRE_CONTAINER (default: pg-parity) with PostgreSQL 18 psql/libpq"]
fn postgresql_18_libpq_negotiates_30_32_latest_and_downgrade() {
    assert_postgresql_18_libpq();
    let cases = [
        ("3.0", ProtocolVersion::V3_2, ProtocolVersion::V3_0),
        ("3.2", ProtocolVersion::V3_2, ProtocolVersion::V3_2),
        ("latest", ProtocolVersion::V3_2, ProtocolVersion::V3_2),
        ("3.2", ProtocolVersion::V3_0, ProtocolVersion::V3_0),
    ];

    for (requested, newest_supported, expected) in cases {
        let (listener, port) = bind_test_listener();
        let server = thread::spawn(move || serve_select_one(&listener, newest_supported));
        let output = run_psql(port, requested, false);
        let observation = server.join().expect("live query server completed");
        assert_psql_success(&output, expected);
        assert_eq!(observation.negotiated, expected);
        assert_eq!(
            observation.startup.version.minor,
            requested_version(requested).minor
        );
        assert!(observation.pre_startup.is_empty());
        assert_eq!(observation.query.trim().trim_end_matches(';'), "SELECT 1");
    }
}

fn requested_version(value: &str) -> ProtocolVersion {
    match value {
        "3.0" => ProtocolVersion::V3_0,
        "3.2" | "latest" => ProtocolVersion::V3_2,
        other => panic!("unsupported live-test protocol version {other}"),
    }
}

fn serve_extended_query(
    listener: &TcpListener,
    newest_supported: ProtocolVersion,
) -> ExtendedQueryObservation {
    let (mut stream, startup, negotiated, _, _) = accept_authenticated(listener, newest_supported);

    let FrontendMessage::Parse(parse) = read_frontend_message(&mut stream) else {
        panic!("psql did not begin the extended query with Parse");
    };
    assert_eq!(
        parse.query.trim().trim_end_matches(';'),
        "SELECT $1::int4 + 1"
    );
    write_backend_messages(&mut stream, negotiated, &[BackendMessage::ParseComplete]);

    let FrontendMessage::Bind(bind) = read_frontend_message(&mut stream) else {
        panic!("psql did not send Bind after Parse");
    };
    assert_eq!(bind.statement, parse.statement);
    assert_eq!(bind.parameters.len(), 1);
    let parameter = bind.parameters[0]
        .clone()
        .expect("psql sent a non-NULL parameter");
    assert_eq!(parameter, b"41");
    assert!(matches!(
        bind.parameter_formats.as_slice(),
        [] | [FormatCode::Text]
    ));
    write_backend_messages(&mut stream, negotiated, &[BackendMessage::BindComplete]);

    let FrontendMessage::Describe(DescribeTarget::Portal(portal)) =
        read_frontend_message(&mut stream)
    else {
        panic!("psql did not describe the bound portal");
    };
    assert_eq!(portal, bind.portal);
    let result_format = match bind.result_formats.as_slice() {
        [] | [FormatCode::Text] => FormatCode::Text,
        [FormatCode::Binary] => FormatCode::Binary,
        formats => panic!("unexpected one-column result formats: {formats:?}"),
    };
    let mut field = FieldDescription::text("?column?", TYPE_INT4, 4);
    field.format = result_format;
    write_backend_messages(
        &mut stream,
        negotiated,
        &[BackendMessage::RowDescription(vec![field])],
    );

    let FrontendMessage::Execute(execute) = read_frontend_message(&mut stream) else {
        panic!("psql did not execute the bound portal");
    };
    assert_eq!(execute.portal, bind.portal);
    assert_eq!(execute.max_rows, 0);
    let value = match result_format {
        FormatCode::Text => b"42".to_vec(),
        FormatCode::Binary => 42_i32.to_be_bytes().to_vec(),
    };
    write_backend_messages(
        &mut stream,
        negotiated,
        &[
            BackendMessage::DataRow(vec![Some(value)]),
            BackendMessage::CommandComplete("SELECT 1".to_owned()),
        ],
    );

    assert_eq!(read_frontend_message(&mut stream), FrontendMessage::Sync);
    write_backend_messages(
        &mut stream,
        negotiated,
        &[BackendMessage::ReadyForQuery(TransactionStatus::Idle)],
    );
    assert_eq!(
        read_frontend_message(&mut stream),
        FrontendMessage::Terminate
    );

    ExtendedQueryObservation {
        startup,
        negotiated,
        query: parse.query,
        parameter,
    }
}

fn run_extended_psql(port: u16, max_protocol_version: &str) -> Output {
    let conninfo = format!(
        "host={} port={port} user=uqa dbname=uqa connect_timeout=5 max_protocol_version={max_protocol_version} gssencmode=disable sslmode=disable",
        docker_host()
    );
    let mut child = Command::new("docker")
        .args([
            "exec",
            "-i",
            &container_name(),
            "psql",
            "-X",
            "-Atq",
            &conninfo,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn PostgreSQL 18 psql extended-query client");
    child
        .stdin
        .take()
        .expect("psql stdin is piped")
        .write_all(b"SELECT $1::int4 + 1 \\bind 41 \\g\n\\q\n")
        .expect("write psql extended-query script");
    child
        .wait_with_output()
        .expect("wait for PostgreSQL 18 psql extended-query client")
}

#[test]
#[ignore = "requires UQA_PG18_DOCKER_HOST and UQA_PG18_WIRE_CONTAINER (default: pg-parity) with PostgreSQL 18 psql/libpq"]
fn postgresql_18_libpq_executes_extended_query_over_protocol_32() {
    assert_postgresql_18_libpq();
    let (listener, port) = bind_test_listener();
    let server = thread::spawn(move || serve_extended_query(&listener, ProtocolVersion::V3_2));
    let output = run_extended_psql(port, "3.2");
    let observation = server.join().expect("extended-query server completed");

    assert!(
        output.status.success(),
        "psql extended query failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == "42"),
        "unexpected psql output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(observation.startup.version, ProtocolVersion::V3_2);
    assert_eq!(observation.negotiated, ProtocolVersion::V3_2);
    assert_eq!(observation.query.trim(), "SELECT $1::int4 + 1");
    assert_eq!(observation.parameter, b"41");
}

#[test]
#[ignore = "requires UQA_PG18_DOCKER_HOST and UQA_PG18_WIRE_CONTAINER (default: pg-parity) with PostgreSQL 18 psql/libpq"]
fn postgresql_18_libpq_retries_after_ssl_rejection() {
    assert_postgresql_18_libpq();
    let (listener, port) = bind_test_listener();
    let server = thread::spawn(move || serve_select_one(&listener, ProtocolVersion::V3_2));
    let output = run_psql(port, "3.2", true);
    let observation = server
        .join()
        .expect("encrypted startup test server completed");
    assert_psql_success(&output, ProtocolVersion::V3_2);
    assert_eq!(observation.pre_startup, vec![PreStartupRequest::Ssl]);
}

fn spawn_cancellable_psql(
    port: u16,
    max_protocol_version: &str,
    pid_file: &str,
) -> std::process::Child {
    let conninfo = format!(
        "host={} port={port} user=uqa dbname=uqa connect_timeout=5 max_protocol_version={max_protocol_version} gssencmode=disable sslmode=disable",
        docker_host()
    );
    Command::new("docker")
        .args([
            "exec",
            &container_name(),
            "sh",
            "-c",
            "echo $$ > \"$1\"; shift; exec \"$@\"",
            "uqa-pg-wire",
            pid_file,
            "psql",
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            &conninfo,
            "-c",
            "\\conninfo",
            "-c",
            "SELECT pg_sleep(30)",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cancellable PostgreSQL 18 psql")
}

fn container_process_id(pid_file: &str) -> String {
    let output = Command::new("docker")
        .args(["exec", &container_name(), "cat", pid_file])
        .output()
        .expect("read container psql process id");
    assert!(output.status.success(), "missing psql pid file {pid_file}");
    String::from_utf8(output.stdout)
        .expect("psql process id is UTF-8")
        .trim()
        .to_owned()
}

fn signal_container_process(signal: &str, process_id: &str) -> Output {
    Command::new("docker")
        .args(["exec", &container_name(), "kill", signal, process_id])
        .output()
        .expect("signal container psql process")
}

fn remove_pid_file(pid_file: &str) {
    let output = Command::new("docker")
        .args(["exec", &container_name(), "rm", "-f", pid_file])
        .output()
        .expect("remove container psql pid file");
    assert!(output.status.success(), "failed to remove {pid_file}");
}

fn serve_cancellation(
    listener: &TcpListener,
    newest_supported: ProtocolVersion,
    query_started: &mpsc::Sender<()>,
    cancel_received: &mpsc::Sender<()>,
) -> CancellationObservation {
    let (mut query_stream, startup, negotiated, expected_key, pre_startup) =
        accept_authenticated(listener, newest_supported);
    let FrontendMessage::Query(query) = read_frontend_message(&mut query_stream) else {
        panic!("psql did not send cancellation probe over simple query protocol");
    };
    assert_eq!(query.trim().trim_end_matches(';'), "SELECT pg_sleep(30)");
    query_started.send(()).expect("report query start");

    let mut cancel_stream = accept_with_timeout(listener);
    let (cancel_frame, cancel_pre_startup) =
        read_startup_after_rejecting_encryption(&mut cancel_stream);
    let StartupFrame::CancelRequest {
        process_id,
        secret_key,
    } = cancel_frame
    else {
        panic!("expected libpq CancelRequest, got {cancel_frame:?}");
    };
    assert_eq!(process_id, BACKEND_PROCESS_ID);
    assert_eq!(secret_key, expected_key);
    drop(cancel_stream);
    cancel_received.send(()).expect("report cancellation");

    let error = ErrorOrNotice::error(
        sqlstate::QUERY_CANCELED,
        "canceling statement due to user request",
    );
    let bytes = encode_all_for_protocol(
        &[
            BackendMessage::ErrorResponse(error),
            BackendMessage::ReadyForQuery(TransactionStatus::Idle),
        ],
        negotiated,
    )
    .expect("encode cancellation response");
    query_stream
        .write_all(&bytes)
        .expect("write cancellation response");
    assert_eq!(
        read_frontend_message(&mut query_stream),
        FrontendMessage::Terminate
    );

    CancellationObservation {
        connection: ConnectionObservation {
            startup,
            negotiated,
            pre_startup,
            query,
        },
        process_id,
        secret_key,
        cancel_pre_startup,
    }
}

#[test]
#[ignore = "requires UQA_PG18_DOCKER_HOST and UQA_PG18_WIRE_CONTAINER (default: pg-parity) with PostgreSQL 18 psql/libpq"]
fn postgresql_18_libpq_cancels_with_legacy_and_256_byte_keys() {
    assert_postgresql_18_libpq();
    for (requested, newest_supported, expected_key_length) in [
        ("3.0", ProtocolVersion::V3_2, 4),
        ("3.2", ProtocolVersion::V3_2, 256),
    ] {
        let (listener, port) = bind_test_listener();
        let (query_tx, query_rx) = mpsc::channel();
        let (cancel_tx, cancel_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            serve_cancellation(&listener, newest_supported, &query_tx, &cancel_tx)
        });
        let pid_file = format!("/tmp/uqa-pg-wire-{port}.pid");
        let mut client = spawn_cancellable_psql(port, requested, &pid_file);
        query_rx
            .recv_timeout(ACCEPT_TIMEOUT)
            .expect("psql sent cancellation probe query");
        let process_id = container_process_id(&pid_file);
        let signal = signal_container_process("-INT", &process_id);
        assert!(
            signal.status.success(),
            "failed to interrupt psql: {}",
            String::from_utf8_lossy(&signal.stderr)
        );
        if let Err(error) = cancel_rx.recv_timeout(ACCEPT_TIMEOUT) {
            let _ = signal_container_process("-TERM", &process_id);
            let _ = client.kill();
            panic!("libpq did not send CancelRequest: {error}");
        }
        let output = client.wait_with_output().expect("wait for cancelled psql");
        remove_pid_file(&pid_file);
        let observation = server.join().expect("cancellation server completed");
        assert!(
            !output.status.success(),
            "cancelled psql unexpectedly succeeded"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("canceling statement due to user request"),
            "unexpected cancellation stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            observation.connection.negotiated,
            requested_version(requested)
        );
        assert_eq!(observation.process_id, BACKEND_PROCESS_ID);
        assert_eq!(observation.secret_key.as_bytes().len(), expected_key_length);
        assert!(observation.connection.pre_startup.is_empty());
        assert!(observation.cancel_pre_startup.is_empty());
    }
}
