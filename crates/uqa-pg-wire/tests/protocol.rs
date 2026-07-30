//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_pg_wire::backend::{sqlstate, TYPE_INT4, TYPE_TEXT};
use uqa_pg_wire::{
    decode_frontend, decode_startup, encode_all, Authentication, BackendKeyData, BackendMessage,
    Bind, CloseTarget, CopyResponse, DescribeTarget, ErrorOrNotice, FieldDescription, FormatCode,
    FrontendMessage, GSSEncResponse, Parse, PgWireError, SSLResponse, StartupFrame,
    TransactionStatus, CANCEL_REQUEST_CODE, GSSENC_REQUEST_CODE, PROTOCOL_VERSION_3_0,
    SSL_REQUEST_CODE,
};

fn startup_packet(code_or_version: i32, body: &[u8]) -> Vec<u8> {
    let length = i32::try_from(8 + body.len()).expect("test packet fits");
    let mut packet = Vec::new();
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(&code_or_version.to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

fn frontend_packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let length = i32::try_from(4 + body.len()).expect("test packet fits");
    let mut packet = Vec::new();
    packet.push(tag);
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

#[test]
fn decodes_startup_parameters_without_network_state() {
    let body = b"user\0jaepil\0database\0uqa\0application_name\0psql\0\0";
    let packet = startup_packet(PROTOCOL_VERSION_3_0, body);

    let Some((StartupFrame::Startup(startup), consumed)) =
        decode_startup(&packet).expect("startup decodes")
    else {
        panic!("expected startup frame");
    };

    assert_eq!(consumed, packet.len());
    assert_eq!(startup.version.major, 3);
    assert_eq!(startup.version.minor, 0);
    assert_eq!(startup.user(), Some("jaepil"));
    assert_eq!(startup.database(), Some("uqa"));
    assert_eq!(startup.application_name(), Some("psql"));
}

#[test]
fn startup_decoder_reports_incomplete_packets() {
    let packet = startup_packet(PROTOCOL_VERSION_3_0, b"user\0jaepil\0\0");
    let truncated = &packet[..packet.len() - 1];

    assert_eq!(decode_startup(truncated).expect("not malformed"), None);
}

#[test]
fn decodes_startup_special_requests() {
    let Some((StartupFrame::SSLRequest, ssl_consumed)) =
        decode_startup(&startup_packet(SSL_REQUEST_CODE, b"")).expect("ssl decodes")
    else {
        panic!("expected ssl request");
    };
    assert_eq!(ssl_consumed, 8);

    let Some((StartupFrame::GSSEncRequest, gss_consumed)) =
        decode_startup(&startup_packet(GSSENC_REQUEST_CODE, b"")).expect("gss decodes")
    else {
        panic!("expected gss request");
    };
    assert_eq!(gss_consumed, 8);

    let mut cancel_body = Vec::new();
    cancel_body.extend_from_slice(&123_i32.to_be_bytes());
    cancel_body.extend_from_slice(&456_i32.to_be_bytes());
    let Some((
        StartupFrame::CancelRequest {
            process_id,
            secret_key,
        },
        consumed,
    )) =
        decode_startup(&startup_packet(CANCEL_REQUEST_CODE, &cancel_body)).expect("cancel decodes")
    else {
        panic!("expected cancel request");
    };
    assert_eq!(consumed, 16);
    assert_eq!(process_id, 123);
    assert_eq!(secret_key, 456);
}

#[test]
fn decodes_simple_query_message() {
    let packet = frontend_packet(b'Q', b"SELECT 1\0");

    let Some((FrontendMessage::Query(query), consumed)) =
        decode_frontend(&packet).expect("query decodes")
    else {
        panic!("expected query message");
    };

    assert_eq!(consumed, packet.len());
    assert_eq!(query, "SELECT 1");
}

#[test]
fn decodes_extended_query_messages() {
    let parse_body = {
        let mut body = Vec::new();
        body.extend_from_slice(b"stmt\0SELECT $1::int4\0");
        body.extend_from_slice(&1_i16.to_be_bytes());
        body.extend_from_slice(&TYPE_INT4.to_be_bytes());
        body
    };
    let Some((FrontendMessage::Parse(parse), _)) =
        decode_frontend(&frontend_packet(b'P', &parse_body)).expect("parse decodes")
    else {
        panic!("expected parse");
    };
    assert_eq!(
        parse,
        Parse {
            statement: "stmt".to_owned(),
            query: "SELECT $1::int4".to_owned(),
            parameter_type_oids: vec![TYPE_INT4],
        }
    );

    let bind_body = {
        let mut body = Vec::new();
        body.extend_from_slice(b"portal\0stmt\0");
        body.extend_from_slice(&1_i16.to_be_bytes());
        body.extend_from_slice(&1_i16.to_be_bytes());
        body.extend_from_slice(&2_i16.to_be_bytes());
        body.extend_from_slice(&2_i32.to_be_bytes());
        body.extend_from_slice(b"42");
        body.extend_from_slice(&(-1_i32).to_be_bytes());
        body.extend_from_slice(&1_i16.to_be_bytes());
        body.extend_from_slice(&0_i16.to_be_bytes());
        body
    };
    let Some((FrontendMessage::Bind(bind), _)) =
        decode_frontend(&frontend_packet(b'B', &bind_body)).expect("bind decodes")
    else {
        panic!("expected bind");
    };
    assert_eq!(
        bind,
        Bind {
            portal: "portal".to_owned(),
            statement: "stmt".to_owned(),
            parameter_formats: vec![FormatCode::Binary],
            parameters: vec![Some(b"42".to_vec()), None],
            result_formats: vec![FormatCode::Text],
        }
    );

    let Some((FrontendMessage::Describe(describe), _)) =
        decode_frontend(&frontend_packet(b'D', b"Pportal\0")).expect("describe decodes")
    else {
        panic!("expected describe");
    };
    assert_eq!(describe, DescribeTarget::Portal("portal".to_owned()));

    let execute_body = {
        let mut body = Vec::new();
        body.extend_from_slice(b"portal\0");
        body.extend_from_slice(&10_i32.to_be_bytes());
        body
    };
    let Some((FrontendMessage::Execute(execute), _)) =
        decode_frontend(&frontend_packet(b'E', &execute_body)).expect("execute decodes")
    else {
        panic!("expected execute");
    };
    assert_eq!(execute.portal, "portal");
    assert_eq!(execute.max_rows, 10);

    let Some((FrontendMessage::Close(close), _)) =
        decode_frontend(&frontend_packet(b'C', b"Sstmt\0")).expect("close decodes")
    else {
        panic!("expected close");
    };
    assert_eq!(close, CloseTarget::Statement("stmt".to_owned()));
}

#[test]
fn rejects_malformed_frontend_lengths() {
    let mut packet = Vec::new();
    packet.push(b'Q');
    packet.extend_from_slice(&3_i32.to_be_bytes());

    let error = decode_frontend(&packet).expect_err("length below four is invalid");
    assert_eq!(
        error,
        PgWireError::InvalidLength {
            length: 3,
            minimum: 4,
        }
    );
}

#[test]
fn rejects_negative_execute_limits_and_mismatched_bind_formats() {
    let mut execute_body = Vec::new();
    execute_body.extend_from_slice(b"portal\0");
    execute_body.extend_from_slice(&(-1_i32).to_be_bytes());
    assert_eq!(
        decode_frontend(&frontend_packet(b'E', &execute_body)),
        Err(PgWireError::NegativeValue {
            context: "Execute max rows",
        })
    );

    let mut bind_body = Vec::new();
    bind_body.extend_from_slice(b"portal\0stmt\0");
    bind_body.extend_from_slice(&2_i16.to_be_bytes());
    bind_body.extend_from_slice(&0_i16.to_be_bytes());
    bind_body.extend_from_slice(&1_i16.to_be_bytes());
    bind_body.extend_from_slice(&1_i16.to_be_bytes());
    assert_eq!(
        decode_frontend(&frontend_packet(b'B', &bind_body)),
        Err(PgWireError::ParameterFormatCountMismatch {
            format_count: 2,
            parameter_count: 1,
        })
    );
}

#[test]
fn encodes_startup_backend_messages() {
    let messages = [
        BackendMessage::Authentication(Authentication::Ok),
        BackendMessage::BackendKeyData(BackendKeyData {
            process_id: 12,
            secret_key: 34,
        }),
        BackendMessage::ReadyForQuery(TransactionStatus::Idle),
    ];

    let encoded = encode_all(&messages).expect("backend messages encode");
    assert_eq!(
        encoded,
        [
            b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'K', 0, 0, 0, 12, 0, 0, 0, 12, 0, 0, 0, 34, b'Z', 0, 0,
            0, 5, b'I',
        ]
    );
}

#[test]
fn preserves_failed_transaction_status_on_the_wire() {
    assert_eq!(
        BackendMessage::ReadyForQuery(TransactionStatus::Failed)
            .encode()
            .expect("failed transaction status encodes"),
        [b'Z', 0, 0, 0, 5, b'E']
    );
    assert_eq!(
        TransactionStatus::from_byte(b'E').expect("failed status decodes"),
        TransactionStatus::Failed
    );
}

#[test]
fn encodes_pre_startup_encryption_responses_as_single_bytes() {
    assert_eq!(SSLResponse::Accept.encode(), [b'S']);
    assert_eq!(SSLResponse::Reject.encode(), [b'N']);
    assert_eq!(GSSEncResponse::Accept.encode(), [b'G']);
    assert_eq!(GSSEncResponse::Reject.encode(), [b'N']);
}

#[test]
fn encodes_row_description_data_row_and_command_complete() {
    let fields = vec![
        FieldDescription::text("id", TYPE_INT4, 4),
        FieldDescription::text("title", TYPE_TEXT, -1),
    ];
    let encoded = encode_all(&[
        BackendMessage::RowDescription(fields),
        BackendMessage::DataRow(vec![Some(b"1".to_vec()), Some(b"hello".to_vec())]),
        BackendMessage::CommandComplete("SELECT 1".to_owned()),
    ])
    .expect("result messages encode");

    assert_eq!(encoded[0], b'T');
    assert!(encoded.windows(3).any(|window| window == b"id\0"));
    assert!(encoded.windows(6).any(|window| window == b"title\0"));
    assert!(encoded.windows(5).any(|window| window == b"hello"));
    assert!(encoded.ends_with(b"SELECT 1\0"));
}

#[test]
fn encodes_error_response_fields() {
    let mut error = ErrorOrNotice::error(sqlstate::SYNTAX_ERROR, "syntax error at or near FROM");
    error.detail = Some("missing select list".to_owned());
    error.position = Some(8);

    let encoded = BackendMessage::ErrorResponse(error)
        .encode()
        .expect("error encodes");

    assert_eq!(encoded[0], b'E');
    assert!(encoded.windows(6).any(|window| window == b"SERROR"));
    assert!(encoded.windows(6).any(|window| window == b"C42601"));
    assert!(encoded.ends_with(&[0, 0]));
}

#[test]
fn backend_encoding_rejects_truncating_nuls_and_invalid_sqlstates() {
    assert_eq!(
        BackendMessage::CommandComplete("SELECT\0 1".to_owned()).encode(),
        Err(PgWireError::EmbeddedNul {
            context: "CommandComplete tag",
        })
    );

    let invalid_code = ErrorOrNotice::error("XX", "engine failure");
    assert_eq!(
        BackendMessage::ErrorResponse(invalid_code).encode(),
        Err(PgWireError::InvalidSqlState {
            code: "XX".to_owned(),
        })
    );

    let invalid_message = ErrorOrNotice::error(sqlstate::INTERNAL_ERROR, "engine\0failure");
    assert_eq!(
        BackendMessage::ErrorResponse(invalid_message).encode(),
        Err(PgWireError::EmbeddedNul {
            context: "error message",
        })
    );
}

#[test]
fn encodes_copy_responses_without_io_runtime() {
    let encoded = BackendMessage::CopyOutResponse(CopyResponse {
        overall_format: FormatCode::Binary,
        column_formats: vec![FormatCode::Text, FormatCode::Binary],
    })
    .encode()
    .expect("copy response encodes");

    assert_eq!(encoded, [b'H', 0, 0, 0, 11, 1, 0, 2, 0, 0, 0, 1]);
}
