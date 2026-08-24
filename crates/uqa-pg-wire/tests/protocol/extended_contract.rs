//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;

use uqa_pg_wire::{
    decode_frontend, Authentication, AuthenticationExchange, AuthenticationResponse,
    AuthenticationResponseKind, BackendMessage, Bind, CancelKey, FormatCode, FrontendMessage,
    FunctionCall, PasswordMessage, PgWireError,
};

fn frontend_packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let length = i32::try_from(4 + body.len()).expect("test packet fits");
    let mut packet = Vec::with_capacity(body.len() + 5);
    packet.push(tag);
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

#[test]
fn bind_resolves_binary_parameter_and_result_formats_for_zero_one_and_n_forms() {
    let parameters = vec![Some(41_i32.to_be_bytes().to_vec()), Some(vec![0, 1, 2])];
    let mut bind = Bind {
        portal: "portal".to_owned(),
        statement: "statement".to_owned(),
        parameter_formats: Vec::new(),
        parameters,
        result_formats: Vec::new(),
    };

    assert_eq!(
        bind.resolved_parameter_formats().unwrap(),
        [FormatCode::Text, FormatCode::Text]
    );
    assert_eq!(
        bind.resolved_result_formats(2).unwrap(),
        [FormatCode::Text, FormatCode::Text]
    );

    bind.parameter_formats = vec![FormatCode::Binary];
    bind.result_formats = vec![FormatCode::Binary];
    assert_eq!(
        bind.resolved_parameter_formats().unwrap(),
        [FormatCode::Binary, FormatCode::Binary]
    );
    assert_eq!(bind.parameter_format(1).unwrap(), FormatCode::Binary);
    assert_eq!(
        bind.resolved_result_formats(2).unwrap(),
        [FormatCode::Binary, FormatCode::Binary]
    );
    assert_eq!(bind.result_format(0, 2).unwrap(), FormatCode::Binary);

    bind.parameter_formats = vec![FormatCode::Binary, FormatCode::Text];
    bind.result_formats = vec![FormatCode::Text, FormatCode::Binary];
    assert_eq!(
        bind.resolved_parameter_formats().unwrap(),
        [FormatCode::Binary, FormatCode::Text]
    );
    assert_eq!(
        bind.resolved_result_formats(2).unwrap(),
        [FormatCode::Text, FormatCode::Binary]
    );
}

#[test]
fn format_resolution_rejects_result_count_mismatches_and_out_of_range_indexes() {
    let bind = Bind {
        portal: String::new(),
        statement: String::new(),
        parameter_formats: vec![FormatCode::Text],
        parameters: vec![Some(b"one".to_vec())],
        result_formats: vec![FormatCode::Text, FormatCode::Binary],
    };
    assert_eq!(
        bind.resolved_result_formats(3),
        Err(PgWireError::ResultFormatCountMismatch {
            format_count: 2,
            column_count: 3,
        })
    );
    assert_eq!(
        bind.parameter_format(1),
        Err(PgWireError::FormatIndexOutOfRange {
            context: "Bind parameter",
            index: 1,
            count: 1,
        })
    );

    let call = FunctionCall {
        function_oid: 42,
        argument_formats: vec![FormatCode::Binary],
        arguments: vec![Some(7_i32.to_be_bytes().to_vec()), None],
        result_format: FormatCode::Binary,
    };
    assert_eq!(
        call.resolved_argument_formats().unwrap(),
        [FormatCode::Binary, FormatCode::Binary]
    );
    assert_eq!(call.argument_format(0).unwrap(), FormatCode::Binary);
}

#[test]
fn authentication_exchange_decodes_cleartext_and_md5_password_responses() {
    for request in [
        Authentication::CleartextPassword,
        Authentication::Md5Password(*b"salt"),
    ] {
        let mut exchange = AuthenticationExchange::new();
        exchange.send(&request).unwrap();
        assert_eq!(
            exchange.awaiting_response(),
            Some(AuthenticationResponseKind::Password)
        );
        assert_eq!(
            exchange
                .receive(&PasswordMessage::new(b"secret\0".to_vec()))
                .unwrap(),
            AuthenticationResponse::Password("secret".to_owned())
        );
        exchange.send(&Authentication::Ok).unwrap();
        assert!(exchange.is_complete());
    }
}

#[test]
fn authentication_exchange_decodes_complete_sasl_gss_and_sspi_sequences() {
    let mut sasl = AuthenticationExchange::new();
    sasl.send(&Authentication::Sasl {
        mechanisms: vec!["SCRAM-SHA-256".to_owned()],
    })
    .unwrap();
    let mut initial = b"SCRAM-SHA-256\0".to_vec();
    initial.extend_from_slice(&3_i32.to_be_bytes());
    initial.extend_from_slice(b"n,,");
    assert_eq!(
        sasl.receive(&PasswordMessage::new(initial)).unwrap(),
        AuthenticationResponse::SaslInitial {
            mechanism: "SCRAM-SHA-256".to_owned(),
            initial_response: Some(b"n,,".to_vec()),
        }
    );
    sasl.send(&Authentication::SaslContinue(b"r=nonce".to_vec()))
        .unwrap();
    assert_eq!(
        sasl.receive(&PasswordMessage::new(b"c=biws".to_vec()))
            .unwrap(),
        AuthenticationResponse::Sasl(b"c=biws".to_vec())
    );
    sasl.send(&Authentication::SaslFinal(b"v=proof".to_vec()))
        .unwrap();
    sasl.send(&Authentication::Ok).unwrap();
    assert!(sasl.is_complete());

    let mut gss = AuthenticationExchange::new();
    gss.send(&Authentication::Gss).unwrap();
    assert_eq!(
        gss.receive(&PasswordMessage::new([0, 1, 2])).unwrap(),
        AuthenticationResponse::Gss(vec![0, 1, 2])
    );
    gss.send(&Authentication::GssContinue(vec![3])).unwrap();
    assert_eq!(
        gss.receive(&PasswordMessage::new([4, 5])).unwrap(),
        AuthenticationResponse::Gss(vec![4, 5])
    );
    gss.send(&Authentication::Ok).unwrap();
    assert!(gss.is_complete());

    let mut sspi = AuthenticationExchange::new();
    sspi.send(&Authentication::Sspi).unwrap();
    assert_eq!(
        sspi.receive(&PasswordMessage::new([6, 7])).unwrap(),
        AuthenticationResponse::Sspi(vec![6, 7])
    );
    sspi.send(&Authentication::GssContinue(vec![8])).unwrap();
    assert_eq!(
        sspi.receive(&PasswordMessage::new([9])).unwrap(),
        AuthenticationResponse::Sspi(vec![9])
    );
    sspi.send(&Authentication::Ok).unwrap();
    assert!(sspi.is_complete());
}

#[test]
fn authentication_exchange_rejects_malformed_or_out_of_order_responses() {
    let mut exchange = AuthenticationExchange::new();
    assert_eq!(
        exchange.receive(&PasswordMessage::new(b"secret\0".to_vec())),
        Err(PgWireError::InvalidAuthenticationSequence {
            state: "ready for an authentication request",
            message: "a frontend authentication response",
        })
    );

    exchange
        .send(&Authentication::Sasl {
            mechanisms: vec!["SCRAM-SHA-256".to_owned()],
        })
        .unwrap();
    let mut truncated = b"SCRAM-SHA-256\0".to_vec();
    truncated.extend_from_slice(&4_i32.to_be_bytes());
    truncated.extend_from_slice(b"abc");
    assert_eq!(
        exchange.receive(&PasswordMessage::new(truncated)),
        Err(PgWireError::UnexpectedEof {
            context: "SASLInitialResponse data",
        })
    );
    assert_eq!(
        BackendMessage::Authentication(Authentication::Sasl {
            mechanisms: vec![String::new()]
        })
        .encode(),
        Err(PgWireError::EmptySaslMechanism)
    );
    assert_eq!(
        BackendMessage::Authentication(Authentication::Sasl {
            mechanisms: Vec::new()
        })
        .encode(),
        Err(PgWireError::EmptySaslMechanismList)
    );

    let mut failed = AuthenticationExchange::new();
    failed.send(&Authentication::CleartextPassword).unwrap();
    failed.fail().unwrap();
    assert!(failed.is_failed());
    assert_eq!(
        failed.receive(&PasswordMessage::new(b"secret\0".to_vec())),
        Err(PgWireError::InvalidAuthenticationSequence {
            state: "authentication has failed",
            message: "a frontend authentication response",
        })
    );
}

#[test]
fn cancellation_keys_support_multiple_bounded_middleware_prefixes() {
    let origin = CancelKey::new(vec![0x44; 32]).unwrap();
    let layer_one = origin.with_middleware_prefix(&[0x11; 96]).unwrap();
    let layer_two = layer_one.with_middleware_prefix(&[0x22; 128]).unwrap();
    assert_eq!(layer_two.as_bytes().len(), 256);
    assert_eq!(
        layer_two.with_middleware_prefix(&[0x33]),
        Err(PgWireError::InvalidCancelKeyLength {
            length: 257,
            minimum: 1,
            maximum: 256,
        })
    );

    let (prefix_two, downstream_one) = layer_two.remove_middleware_prefix(128).unwrap();
    assert_eq!(prefix_two, vec![0x22; 128]);
    let (prefix_one, downstream_origin) = downstream_one.remove_middleware_prefix(96).unwrap();
    assert_eq!(prefix_one, vec![0x11; 96]);
    assert_eq!(downstream_origin, origin);
    assert_eq!(
        origin.remove_middleware_prefix(32),
        Err(PgWireError::InvalidCancelKeyLayerLength {
            layer_length: 32,
            key_length: 32,
        })
    );
}

#[test]
fn live_peer_fragmentation_and_malformed_frames_use_the_same_bounded_decoder() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        decode_frontend(&bytes)
    });

    let mut client = TcpStream::connect(address).unwrap();
    let malformed = frontend_packet(b'B', b"portal-without-nul");
    for fragment in malformed.chunks(3) {
        client.write_all(fragment).unwrap();
    }
    client.shutdown(Shutdown::Write).unwrap();
    assert_eq!(
        server.join().unwrap(),
        Err(PgWireError::MissingNul {
            context: "Bind portal name",
        })
    );
}

#[test]
fn frontend_password_tag_preserves_binary_authentication_payload_until_context_is_known() {
    let packet = frontend_packet(b'p', &[0, 255, 1, 0]);
    let Some((FrontendMessage::Password(message), consumed)) = decode_frontend(&packet).unwrap()
    else {
        panic!("expected PasswordMessage");
    };
    assert_eq!(consumed, packet.len());
    assert_eq!(message.as_bytes(), [0, 255, 1, 0]);
}
