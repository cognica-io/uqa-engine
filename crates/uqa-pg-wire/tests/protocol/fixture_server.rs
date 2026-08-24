//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use uqa_pg_wire::backend::{sqlstate, TYPE_INT4, TYPE_INT8, TYPE_TEXT};
use uqa_pg_wire::{
    decode_frontend, decode_startup, encode_all_for_protocol, Authentication,
    AuthenticationExchange, AuthenticationResponse, BackendKeyData, BackendMessage, Bind,
    CancelKey, CloseTarget, CopyResponse, DescribeTarget, ErrorOrNotice, Execute, FieldDescription,
    FormatCode, FrontendMessage, Parse, PgWireError, ProtocolVersion, StartupFrame,
    TransactionStatus,
};

const IO_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MatrixEvent {
    PasswordAuthentication,
    BinaryParameter,
    BinaryResult,
    PreparedReuse,
    CopyIn,
    CopyOut,
    FailedTransaction,
    RollbackRecovery,
}

#[derive(Debug, Default)]
pub(crate) struct MatrixObservation {
    events: BTreeSet<MatrixEvent>,
}

impl MatrixObservation {
    fn record(&mut self, event: MatrixEvent) {
        self.events.insert(event);
    }

    pub(crate) fn observed(&self, event: MatrixEvent) -> bool {
        self.events.contains(&event)
    }
}

#[derive(Debug, Clone)]
struct PreparedStatement {
    query: String,
    parameter_oids: Vec<u32>,
}

#[derive(Debug, Clone)]
struct BoundPortal {
    statement: PreparedStatement,
    bind: Bind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueryCycle {
    Simple,
    Extended,
}

struct FixtureSession {
    protocol: ProtocolVersion,
    transaction: TransactionStatus,
    statements: HashMap<String, PreparedStatement>,
    portals: HashMap<String, BoundPortal>,
    bind_counts: HashMap<String, usize>,
    skip_until_sync: bool,
    copy_in_cycle: Option<QueryCycle>,
    copy_chunks: usize,
    observation: MatrixObservation,
}

impl FixtureSession {
    fn new(protocol: ProtocolVersion) -> Self {
        Self {
            protocol,
            transaction: TransactionStatus::Idle,
            statements: HashMap::new(),
            portals: HashMap::new(),
            bind_counts: HashMap::new(),
            skip_until_sync: false,
            copy_in_cycle: None,
            copy_chunks: 0,
            observation: MatrixObservation::default(),
        }
    }

    fn send(&self, stream: &mut TcpStream, messages: &[BackendMessage]) {
        let bytes = encode_all_for_protocol(messages, self.protocol).unwrap();
        stream.write_all(&bytes).unwrap();
    }

    fn handle(&mut self, stream: &mut TcpStream, message: FrontendMessage) -> bool {
        if self.skip_until_sync {
            return match message {
                FrontendMessage::Sync => {
                    self.skip_until_sync = false;
                    self.send(stream, &[BackendMessage::ReadyForQuery(self.transaction)]);
                    true
                }
                FrontendMessage::Terminate => false,
                _ => true,
            };
        }

        if let Some(cycle) = self.copy_in_cycle {
            return self.handle_copy_in(stream, cycle, message);
        }

        match message {
            FrontendMessage::Query(query) => self.handle_simple_query(stream, query),
            FrontendMessage::Parse(parse) => self.handle_parse(stream, parse),
            FrontendMessage::Bind(bind) => self.handle_bind(stream, bind),
            FrontendMessage::Describe(target) => self.handle_describe(stream, target),
            FrontendMessage::Execute(execute) => self.handle_execute(stream, &execute),
            FrontendMessage::Close(target) => self.handle_close(stream, target),
            FrontendMessage::Sync => {
                self.send(stream, &[BackendMessage::ReadyForQuery(self.transaction)]);
                true
            }
            FrontendMessage::Flush => true,
            FrontendMessage::Terminate => false,
            other => panic!("unexpected frontend message outside COPY/authentication: {other:?}"),
        }
    }

    fn handle_simple_query(&mut self, stream: &mut TcpStream, query: String) -> bool {
        let statement = PreparedStatement {
            parameter_oids: parameter_oids(&query),
            query,
        };
        self.execute_query(stream, &statement, None, QueryCycle::Simple)
    }

    fn handle_parse(&mut self, stream: &mut TcpStream, parse: Parse) -> bool {
        if self.transaction == TransactionStatus::Failed
            && normalize_query(&parse.query) != "rollback"
        {
            self.send_error(
                stream,
                "25P02",
                "current transaction is aborted, commands ignored until end of transaction block",
                QueryCycle::Extended,
            );
            return true;
        }
        let parameter_oids =
            if parse.parameter_type_oids.is_empty() || parse.parameter_type_oids.contains(&0) {
                parameter_oids(&parse.query)
            } else {
                parse.parameter_type_oids
            };
        self.statements.insert(
            parse.statement,
            PreparedStatement {
                query: parse.query,
                parameter_oids,
            },
        );
        self.send(stream, &[BackendMessage::ParseComplete]);
        true
    }

    fn handle_bind(&mut self, stream: &mut TcpStream, bind: Bind) -> bool {
        let statement = self
            .statements
            .get(&bind.statement)
            .unwrap_or_else(|| panic!("unknown prepared statement {:?}", bind.statement))
            .clone();
        bind.resolved_parameter_formats().unwrap();
        let count = self.bind_counts.entry(bind.statement.clone()).or_default();
        *count += 1;
        if !bind.statement.is_empty() && *count > 1 {
            self.observation.record(MatrixEvent::PreparedReuse);
        }
        if (0..bind.parameters.len()).any(|index| {
            bind.parameter_format(index).unwrap() == FormatCode::Binary
                && bind.parameters[index].is_some()
        }) {
            self.observation.record(MatrixEvent::BinaryParameter);
        }
        let result_count = row_description(&statement.query, None).map_or(0, |fields| fields.len());
        if bind
            .resolved_result_formats(result_count)
            .unwrap()
            .contains(&FormatCode::Binary)
        {
            self.observation.record(MatrixEvent::BinaryResult);
        }
        self.portals
            .insert(bind.portal.clone(), BoundPortal { statement, bind });
        self.send(stream, &[BackendMessage::BindComplete]);
        true
    }

    fn handle_describe(&mut self, stream: &mut TcpStream, target: DescribeTarget) -> bool {
        match target {
            DescribeTarget::Statement(name) => {
                let statement = self
                    .statements
                    .get(&name)
                    .unwrap_or_else(|| panic!("unknown described statement {name:?}"));
                let mut messages = vec![BackendMessage::ParameterDescription(
                    statement.parameter_oids.clone(),
                )];
                match row_description(&statement.query, None) {
                    Some(fields) => messages.push(BackendMessage::RowDescription(fields)),
                    None => messages.push(BackendMessage::NoData),
                }
                self.send(stream, &messages);
            }
            DescribeTarget::Portal(name) => {
                let portal = self
                    .portals
                    .get(&name)
                    .unwrap_or_else(|| panic!("unknown described portal {name:?}"));
                match row_description(&portal.statement.query, Some(&portal.bind)) {
                    Some(fields) => {
                        if fields
                            .iter()
                            .any(|field| field.format == FormatCode::Binary)
                        {
                            self.observation.record(MatrixEvent::BinaryResult);
                        }
                        self.send(stream, &[BackendMessage::RowDescription(fields)]);
                    }
                    None => self.send(stream, &[BackendMessage::NoData]),
                }
            }
        }
        true
    }

    fn handle_execute(&mut self, stream: &mut TcpStream, execute: &Execute) -> bool {
        let portal = self
            .portals
            .get(&execute.portal)
            .unwrap_or_else(|| panic!("unknown executed portal {:?}", execute.portal))
            .clone();
        self.execute_query(
            stream,
            &portal.statement,
            Some(&portal.bind),
            QueryCycle::Extended,
        )
    }

    fn handle_close(&mut self, stream: &mut TcpStream, target: CloseTarget) -> bool {
        match target {
            CloseTarget::Statement(name) => {
                self.statements.remove(&name);
            }
            CloseTarget::Portal(name) => {
                self.portals.remove(&name);
            }
        }
        self.send(stream, &[BackendMessage::CloseComplete]);
        true
    }

    fn execute_query(
        &mut self,
        stream: &mut TcpStream,
        statement: &PreparedStatement,
        bind: Option<&Bind>,
        cycle: QueryCycle,
    ) -> bool {
        let query = normalize_query(&statement.query);
        if self.transaction == TransactionStatus::Failed && query != "rollback" {
            self.send_error(
                stream,
                "25P02",
                "current transaction is aborted, commands ignored until end of transaction block",
                cycle,
            );
            return true;
        }

        if self.try_execute_copy(stream, &query, cycle) {
            return true;
        }

        let mut messages = Vec::new();
        if cycle == QueryCycle::Simple {
            if let Some(fields) = row_description(&statement.query, bind) {
                messages.push(BackendMessage::RowDescription(fields));
            }
        }
        match query.as_str() {
            "begin" => {
                self.transaction = TransactionStatus::InTransaction;
                messages.push(BackendMessage::CommandComplete("BEGIN".to_owned()));
            }
            "rollback" => {
                self.transaction = TransactionStatus::Idle;
                self.observation.record(MatrixEvent::RollbackRecovery);
                messages.push(BackendMessage::CommandComplete("ROLLBACK".to_owned()));
            }
            "deallocate all" => {
                self.statements.clear();
                self.portals.clear();
                self.bind_counts.clear();
                messages.push(BackendMessage::CommandComplete("DEALLOCATE ALL".to_owned()));
            }
            "select 1 / 0" => {
                if self.transaction == TransactionStatus::InTransaction {
                    self.transaction = TransactionStatus::Failed;
                    self.observation.record(MatrixEvent::FailedTransaction);
                }
                self.send_error(stream, "22012", "division by zero", cycle);
                return true;
            }
            "create temp table matrix_copy (id int4, value text)" => {
                messages.push(BackendMessage::CommandComplete("CREATE TABLE".to_owned()));
            }
            "select count(*)::int8 from matrix_copy" => {
                let format = result_format(bind, 0, 1);
                messages.push(BackendMessage::DataRow(vec![Some(encode_i64(2, format))]));
                messages.push(BackendMessage::CommandComplete("SELECT 1".to_owned()));
            }
            "select \"id\", \"value\" from \"matrix_copy\"" => {
                messages.push(BackendMessage::CommandComplete("SELECT 0".to_owned()));
            }
            "select 1" => {
                let format = result_format(bind, 0, 1);
                messages.push(BackendMessage::DataRow(vec![Some(encode_i32(1, format))]));
                messages.push(BackendMessage::CommandComplete("SELECT 1".to_owned()));
            }
            "select $1::int4 + 1 as value" => {
                let bind = bind.expect("parameter query requires Bind");
                let input = decode_i32(bind, 0).unwrap();
                let format = result_format(Some(bind), 0, 1);
                messages.push(BackendMessage::DataRow(vec![Some(encode_i32(
                    input + 1,
                    format,
                ))]));
                messages.push(BackendMessage::CommandComplete("SELECT 1".to_owned()));
            }
            other => panic!("unsupported client-matrix query: {other:?}"),
        }
        if cycle == QueryCycle::Simple {
            messages.push(BackendMessage::ReadyForQuery(self.transaction));
        }
        self.send(stream, &messages);
        true
    }

    fn try_execute_copy(&mut self, stream: &mut TcpStream, query: &str, cycle: QueryCycle) -> bool {
        let is_copy_in = query.starts_with("copy matrix_copy from stdin")
            || (query.starts_with("copy \"matrix_copy\"") && query.contains("from stdin"));
        if is_copy_in {
            let format = if query.contains("binary") {
                FormatCode::Binary
            } else {
                FormatCode::Text
            };
            self.send(
                stream,
                &[BackendMessage::CopyInResponse(CopyResponse {
                    overall_format: format,
                    column_formats: vec![format; 2],
                })],
            );
            self.copy_in_cycle = Some(cycle);
            self.copy_chunks = 0;
            return true;
        }
        if query != "copy matrix_copy to stdout" {
            return false;
        }
        self.observation.record(MatrixEvent::CopyOut);
        let mut messages = vec![
            BackendMessage::CopyOutResponse(CopyResponse {
                overall_format: FormatCode::Text,
                column_formats: vec![FormatCode::Text, FormatCode::Text],
            }),
            BackendMessage::CopyData(b"1\tone\n".to_vec()),
            BackendMessage::CopyData(b"2\ttwo\n".to_vec()),
            BackendMessage::CopyDone,
            BackendMessage::CommandComplete("COPY 2".to_owned()),
        ];
        if cycle == QueryCycle::Simple {
            messages.push(BackendMessage::ReadyForQuery(self.transaction));
        }
        self.send(stream, &messages);
        true
    }

    fn send_error(&mut self, stream: &mut TcpStream, code: &str, message: &str, cycle: QueryCycle) {
        let mut messages = vec![BackendMessage::ErrorResponse(ErrorOrNotice::error(
            code, message,
        ))];
        if cycle == QueryCycle::Simple {
            messages.push(BackendMessage::ReadyForQuery(self.transaction));
        } else {
            self.skip_until_sync = true;
        }
        self.send(stream, &messages);
    }

    fn handle_copy_in(
        &mut self,
        stream: &mut TcpStream,
        cycle: QueryCycle,
        message: FrontendMessage,
    ) -> bool {
        match message {
            FrontendMessage::CopyData(bytes) => {
                if !bytes.is_empty() {
                    self.copy_chunks += 1;
                }
            }
            FrontendMessage::CopyDone => {
                self.observation.record(MatrixEvent::CopyIn);
                self.copy_in_cycle = None;
                let mut messages = vec![BackendMessage::CommandComplete("COPY 2".to_owned())];
                if cycle == QueryCycle::Simple {
                    messages.push(BackendMessage::ReadyForQuery(self.transaction));
                }
                self.send(stream, &messages);
            }
            FrontendMessage::CopyFail(message) => {
                self.copy_in_cycle = None;
                self.send_error(stream, sqlstate::QUERY_CANCELED, &message, cycle);
            }
            FrontendMessage::Flush => {}
            other => panic!("unexpected COPY FROM message: {other:?}"),
        }
        true
    }
}

pub(crate) fn serve_matrix(listener: &TcpListener) -> MatrixObservation {
    let (mut stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
    stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
    let startup = loop {
        match read_startup(&mut stream) {
            StartupFrame::SSLRequest | StartupFrame::GSSEncRequest => {
                stream.write_all(b"N").unwrap();
            }
            StartupFrame::Startup(startup) => break startup,
            other @ StartupFrame::CancelRequest { .. } => {
                panic!("unexpected client-matrix startup frame: {other:?}");
            }
        }
    };
    let negotiation = startup.negotiate(&[]).unwrap();
    let mut startup_messages = Vec::new();
    if let Some(message) = negotiation.response() {
        startup_messages.push(message);
    }
    let mut authentication = AuthenticationExchange::new();
    authentication
        .send(&Authentication::CleartextPassword)
        .unwrap();
    startup_messages.push(BackendMessage::Authentication(
        Authentication::CleartextPassword,
    ));
    let bytes = encode_all_for_protocol(&startup_messages, negotiation.negotiated_version).unwrap();
    stream.write_all(&bytes).unwrap();
    let FrontendMessage::Password(password) = read_frontend(&mut stream).unwrap() else {
        panic!("client did not answer AuthenticationCleartextPassword");
    };
    assert_eq!(
        authentication.receive(&password).unwrap(),
        AuthenticationResponse::Password("matrix-password".to_owned())
    );
    authentication.send(&Authentication::Ok).unwrap();

    let startup_messages = [
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
            name: "DateStyle".to_owned(),
            value: "ISO, MDY".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "integer_datetimes".to_owned(),
            value: "on".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "standard_conforming_strings".to_owned(),
            value: "on".to_owned(),
        },
        BackendMessage::ParameterStatus {
            name: "TimeZone".to_owned(),
            value: "UTC".to_owned(),
        },
        BackendMessage::BackendKeyData(BackendKeyData {
            process_id: 83_219,
            secret_key: CancelKey::from_i32(91_337),
        }),
        BackendMessage::ReadyForQuery(TransactionStatus::Idle),
    ];
    let bytes = encode_all_for_protocol(&startup_messages, negotiation.negotiated_version).unwrap();
    stream.write_all(&bytes).unwrap();

    let mut session = FixtureSession::new(negotiation.negotiated_version);
    if authentication.is_complete() {
        session
            .observation
            .record(MatrixEvent::PasswordAuthentication);
    }
    while let Some(message) = read_frontend(&mut stream) {
        if !session.handle(&mut stream, message) {
            break;
        }
    }
    session.observation
}

fn read_startup(stream: &mut TcpStream) -> StartupFrame {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).unwrap();
    let length = usize::try_from(u32::from_be_bytes(length)).unwrap();
    let mut packet = vec![0_u8; length];
    packet[..4].copy_from_slice(&u32::try_from(length).unwrap().to_be_bytes());
    stream.read_exact(&mut packet[4..]).unwrap();
    let Some((frame, consumed)) = decode_startup(&packet).unwrap() else {
        panic!("complete startup decoded as incomplete");
    };
    assert_eq!(consumed, packet.len());
    frame
}

fn read_frontend(stream: &mut TcpStream) -> Option<FrontendMessage> {
    let mut header = [0_u8; 5];
    match stream.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return None,
        Err(error) => panic!("read client-matrix frontend header: {error}"),
    }
    let body_length =
        usize::try_from(u32::from_be_bytes(header[1..5].try_into().unwrap())).unwrap();
    assert!(body_length >= 4);
    let mut packet = vec![0_u8; body_length + 1];
    packet[..5].copy_from_slice(&header);
    stream.read_exact(&mut packet[5..]).unwrap();
    let Some((message, consumed)) = decode_frontend(&packet).unwrap() else {
        panic!("complete frontend message decoded as incomplete");
    };
    assert_eq!(consumed, packet.len());
    Some(message)
}

fn normalize_query(query: &str) -> String {
    query
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn parameter_oids(query: &str) -> Vec<u32> {
    if normalize_query(query) == "select $1::int4 + 1 as value" {
        vec![TYPE_INT4]
    } else {
        Vec::new()
    }
}

fn row_description(query: &str, bind: Option<&Bind>) -> Option<Vec<FieldDescription>> {
    let normalized = normalize_query(query);
    let (name, oid, size) = match normalized.as_str() {
        "select $1::int4 + 1 as value" => ("value", TYPE_INT4, 4),
        "select 1" | "select 1 / 0" => ("?column?", TYPE_INT4, 4),
        "select count(*)::int8 from matrix_copy" => ("count", TYPE_INT8, 8),
        "select \"id\", \"value\" from \"matrix_copy\"" => {
            let mut id = FieldDescription::text("id", TYPE_INT4, 4);
            let mut value = FieldDescription::text("value", TYPE_TEXT, -1);
            if let Some(bind) = bind {
                id.format = bind.result_format(0, 2).unwrap();
                value.format = bind.result_format(1, 2).unwrap();
            }
            return Some(vec![id, value]);
        }
        _ => return None,
    };
    let mut field = FieldDescription::text(name, oid, size);
    field.format = result_format(bind, 0, 1);
    Some(vec![field])
}

fn result_format(bind: Option<&Bind>, index: usize, count: usize) -> FormatCode {
    bind.map_or(FormatCode::Text, |bind| {
        bind.result_format(index, count).unwrap()
    })
}

fn decode_i32(bind: &Bind, index: usize) -> Result<i32, PgWireError> {
    let bytes = bind.parameters[index]
        .as_deref()
        .expect("matrix parameter is not NULL");
    match bind.parameter_format(index)? {
        FormatCode::Text => Ok(std::str::from_utf8(bytes).unwrap().parse::<i32>().unwrap()),
        FormatCode::Binary => match bytes.len() {
            2 => Ok(i32::from(i16::from_be_bytes(bytes.try_into().unwrap()))),
            4 => Ok(i32::from_be_bytes(bytes.try_into().unwrap())),
            8 => Ok(i32::try_from(i64::from_be_bytes(bytes.try_into().unwrap())).unwrap()),
            width => panic!("unexpected binary integer width: {width}"),
        },
    }
}

fn encode_i32(value: i32, format: FormatCode) -> Vec<u8> {
    match format {
        FormatCode::Text => value.to_string().into_bytes(),
        FormatCode::Binary => value.to_be_bytes().to_vec(),
    }
}

fn encode_i64(value: i64, format: FormatCode) -> Vec<u8> {
    match format {
        FormatCode::Text => value.to_string().into_bytes(),
        FormatCode::Binary => value.to_be_bytes().to_vec(),
    }
}
