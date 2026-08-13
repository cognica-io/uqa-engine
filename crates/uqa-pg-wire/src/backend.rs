//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::codec::{i16_len, i32_len, Writer};
use crate::protocol::{CancelKey, FormatCode, PgWireError, ProtocolVersion, TransactionStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authentication {
    Ok,
    KerberosV5,
    CleartextPassword,
    Md5Password([u8; 4]),
    Sasl { mechanisms: Vec<String> },
    SaslContinue(Vec<u8>),
    SaslFinal(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SSLResponse {
    Accept,
    Reject,
}

impl SSLResponse {
    pub const fn encode(self) -> [u8; 1] {
        match self {
            Self::Accept => *b"S",
            Self::Reject => *b"N",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GSSEncResponse {
    Accept,
    Reject,
}

impl GSSEncResponse {
    pub const fn encode(self) -> [u8; 1] {
        match self {
            Self::Accept => *b"G",
            Self::Reject => *b"N",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendKeyData {
    pub process_id: i32,
    pub secret_key: CancelKey,
}

impl BackendKeyData {
    #[must_use]
    pub fn legacy(process_id: i32, secret_key: i32) -> Self {
        Self {
            process_id,
            secret_key: CancelKey::from_i32(secret_key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendMessage {
    Authentication(Authentication),
    BackendKeyData(BackendKeyData),
    NegotiateProtocolVersion {
        newest_protocol_version: ProtocolVersion,
        unrecognized_options: Vec<String>,
    },
    ParameterStatus {
        name: String,
        value: String,
    },
    ReadyForQuery(TransactionStatus),
    RowDescription(Vec<FieldDescription>),
    DataRow(Vec<Option<Vec<u8>>>),
    CommandComplete(String),
    EmptyQueryResponse,
    ErrorResponse(ErrorOrNotice),
    NoticeResponse(ErrorOrNotice),
    ParseComplete,
    BindComplete,
    CloseComplete,
    NoData,
    ParameterDescription(Vec<u32>),
    PortalSuspended,
    CopyInResponse(CopyResponse),
    CopyOutResponse(CopyResponse),
    CopyBothResponse(CopyResponse),
    CopyData(Vec<u8>),
    CopyDone,
    CopyFail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescription {
    pub name: String,
    pub table_oid: u32,
    pub column_attribute_number: i16,
    pub type_oid: u32,
    pub type_size: i16,
    pub type_modifier: i32,
    pub format: FormatCode,
}

impl FieldDescription {
    pub fn text(name: impl Into<String>, type_oid: u32, type_size: i16) -> Self {
        Self {
            name: name.into(),
            table_oid: 0,
            column_attribute_number: 0,
            type_oid,
            type_size,
            type_modifier: -1,
            format: FormatCode::Text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyResponse {
    pub overall_format: FormatCode,
    pub column_formats: Vec<FormatCode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    Error,
    Fatal,
    Panic,
    Warning,
    Notice,
    Debug,
    Info,
    Log,
}

impl NoticeSeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
            Self::Panic => "PANIC",
            Self::Warning => "WARNING",
            Self::Notice => "NOTICE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Log => "LOG",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorOrNotice {
    pub severity: NoticeSeverity,
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub hint: Option<String>,
    pub position: Option<i32>,
    pub where_: Option<String>,
    pub schema: Option<String>,
    pub table: Option<String>,
    pub column: Option<String>,
    pub data_type: Option<String>,
    pub constraint: Option<String>,
    pub file: Option<String>,
    pub line: Option<i32>,
    pub routine: Option<String>,
}

impl ErrorOrNotice {
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Error,
            code: code.into(),
            message: message.into(),
            detail: None,
            hint: None,
            position: None,
            where_: None,
            schema: None,
            table: None,
            column: None,
            data_type: None,
            constraint: None,
            file: None,
            line: None,
            routine: None,
        }
    }
}

impl BackendMessage {
    pub fn encode(&self) -> Result<Vec<u8>, PgWireError> {
        self.encode_for_protocol(ProtocolVersion::LATEST)
    }

    pub fn encode_for_protocol(
        &self,
        protocol_version: ProtocolVersion,
    ) -> Result<Vec<u8>, PgWireError> {
        match self {
            Self::Authentication(auth) => encode_authentication(auth),
            Self::BackendKeyData(data) => encode_backend_key_data(data, protocol_version),
            Self::NegotiateProtocolVersion {
                newest_protocol_version,
                unrecognized_options,
            } => encode_negotiate_protocol_version(*newest_protocol_version, unrecognized_options),
            Self::ParameterStatus { name, value } => encode_parameter_status(name, value),
            Self::ReadyForQuery(status) => encode_ready_for_query(*status),
            Self::RowDescription(fields) => encode_row_description(fields),
            Self::DataRow(values) => encode_data_row(values),
            Self::CommandComplete(tag) => encode_command_complete(tag),
            Self::EmptyQueryResponse => encode_empty_body(b'I'),
            Self::ErrorResponse(error) => encode_error_or_notice(b'E', error),
            Self::NoticeResponse(notice) => encode_error_or_notice(b'N', notice),
            Self::ParseComplete => encode_empty_body(b'1'),
            Self::BindComplete => encode_empty_body(b'2'),
            Self::CloseComplete => encode_empty_body(b'3'),
            Self::NoData => encode_empty_body(b'n'),
            Self::ParameterDescription(oids) => encode_parameter_description(oids),
            Self::PortalSuspended => encode_empty_body(b's'),
            Self::CopyInResponse(response) => encode_copy_response(b'G', response),
            Self::CopyOutResponse(response) => encode_copy_response(b'H', response),
            Self::CopyBothResponse(response) => encode_copy_response(b'W', response),
            Self::CopyData(bytes) => Writer::frame(b'd', bytes),
            Self::CopyDone => encode_empty_body(b'c'),
            Self::CopyFail(message) => encode_copy_fail(message),
        }
    }
}

pub fn encode_all(messages: &[BackendMessage]) -> Result<Vec<u8>, PgWireError> {
    encode_all_for_protocol(messages, ProtocolVersion::LATEST)
}

pub fn encode_all_for_protocol(
    messages: &[BackendMessage],
    protocol_version: ProtocolVersion,
) -> Result<Vec<u8>, PgWireError> {
    let mut out = Vec::new();
    for message in messages {
        out.extend(message.encode_for_protocol(protocol_version)?);
    }
    Ok(out)
}

pub const fn encode_ssl_response(response: SSLResponse) -> [u8; 1] {
    response.encode()
}

pub const fn encode_gssenc_response(response: GSSEncResponse) -> [u8; 1] {
    response.encode()
}

pub const TYPE_BOOL: u32 = 16;
pub const TYPE_BYTEA: u32 = 17;
pub const TYPE_INT8: u32 = 20;
pub const TYPE_INT2: u32 = 21;
pub const TYPE_INT4: u32 = 23;
pub const TYPE_TEXT: u32 = 25;
pub const TYPE_FLOAT4: u32 = 700;
pub const TYPE_FLOAT8: u32 = 701;
pub const TYPE_VARCHAR: u32 = 1_043;
pub const TYPE_DATE: u32 = 1_082;
pub const TYPE_TIMESTAMP: u32 = 1_114;
pub const TYPE_TIMESTAMPTZ: u32 = 1_184;
pub const TYPE_JSON: u32 = 114;
pub const TYPE_JSONB: u32 = 3_802;

pub mod sqlstate {
    pub const SUCCESSFUL_COMPLETION: &str = "00000";
    pub const WARNING: &str = "01000";
    pub const PROTOCOL_VIOLATION: &str = "08P01";
    pub const FEATURE_NOT_SUPPORTED: &str = "0A000";
    pub const INVALID_PARAMETER_VALUE: &str = "22023";
    pub const QUERY_CANCELED: &str = "57014";
    pub const SYNTAX_ERROR: &str = "42601";
    pub const UNDEFINED_TABLE: &str = "42P01";
    pub const INTERNAL_ERROR: &str = "XX000";
}

fn encode_authentication(auth: &Authentication) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    match auth {
        Authentication::Ok => body.write_i32(0),
        Authentication::KerberosV5 => body.write_i32(2),
        Authentication::CleartextPassword => body.write_i32(3),
        Authentication::Md5Password(salt) => {
            body.write_i32(5);
            body.write_bytes(salt);
        }
        Authentication::Sasl { mechanisms } => {
            body.write_i32(10);
            for mechanism in mechanisms {
                body.write_cstring(mechanism, "SASL mechanism")?;
            }
            body.write_byte(0);
        }
        Authentication::SaslContinue(data) => {
            body.write_i32(11);
            body.write_bytes(data);
        }
        Authentication::SaslFinal(data) => {
            body.write_i32(12);
            body.write_bytes(data);
        }
    }
    Writer::frame(b'R', &body.into_inner())
}

fn encode_backend_key_data(
    data: &BackendKeyData,
    protocol_version: ProtocolVersion,
) -> Result<Vec<u8>, PgWireError> {
    data.secret_key
        .validate_for_backend_key_data(protocol_version)?;
    let mut body = Writer::new();
    body.write_i32(data.process_id);
    body.write_bytes(data.secret_key.as_bytes());
    Writer::frame(b'K', &body.into_inner())
}

fn encode_negotiate_protocol_version(
    newest_protocol_version: ProtocolVersion,
    unrecognized_options: &[String],
) -> Result<Vec<u8>, PgWireError> {
    if newest_protocol_version.negotiate()? != newest_protocol_version {
        return Err(PgWireError::UnsupportedProtocolVersion(
            newest_protocol_version.raw(),
        ));
    }
    let mut body = Writer::new();
    body.write_i32(newest_protocol_version.raw());
    body.write_i32(i32_len(
        unrecognized_options.len(),
        "NegotiateProtocolVersion option count",
    )?);
    for option in unrecognized_options {
        body.write_cstring(option, "NegotiateProtocolVersion option")?;
    }
    Writer::frame(b'v', &body.into_inner())
}

fn encode_parameter_status(name: &str, value: &str) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_cstring(name, "ParameterStatus name")?;
    body.write_cstring(value, "ParameterStatus value")?;
    Writer::frame(b'S', &body.into_inner())
}

fn encode_ready_for_query(status: TransactionStatus) -> Result<Vec<u8>, PgWireError> {
    Writer::frame(b'Z', &[status.as_byte()])
}

fn encode_row_description(fields: &[FieldDescription]) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_i16(i16_len(fields.len(), "RowDescription field")?);
    for field in fields {
        body.write_cstring(&field.name, "RowDescription field name")?;
        body.write_u32(field.table_oid);
        body.write_i16(field.column_attribute_number);
        body.write_u32(field.type_oid);
        body.write_i16(field.type_size);
        body.write_i32(field.type_modifier);
        body.write_format(field.format);
    }
    Writer::frame(b'T', &body.into_inner())
}

fn encode_data_row(values: &[Option<Vec<u8>>]) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_i16(i16_len(values.len(), "DataRow column")?);
    for value in values {
        match value {
            Some(bytes) => {
                body.write_i32(i32_len(bytes.len(), "DataRow value")?);
                body.write_bytes(bytes);
            }
            None => body.write_i32(-1),
        }
    }
    Writer::frame(b'D', &body.into_inner())
}

fn encode_command_complete(tag: &str) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_cstring(tag, "CommandComplete tag")?;
    Writer::frame(b'C', &body.into_inner())
}

fn encode_error_or_notice(tag: u8, message: &ErrorOrNotice) -> Result<Vec<u8>, PgWireError> {
    if message.code.len() != 5
        || !message
            .code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(PgWireError::InvalidSqlState {
            code: message.code.clone(),
        });
    }
    let mut body = Writer::new();
    write_field(&mut body, b'S', message.severity.as_str(), "severity")?;
    write_field(&mut body, b'V', message.severity.as_str(), "severity")?;
    write_field(&mut body, b'C', &message.code, "SQLSTATE")?;
    write_field(&mut body, b'M', &message.message, "error message")?;
    write_optional_field(&mut body, b'D', message.detail.as_deref(), "error detail")?;
    write_optional_field(&mut body, b'H', message.hint.as_deref(), "error hint")?;
    write_optional_i32_field(&mut body, b'P', message.position, "error position")?;
    write_optional_field(&mut body, b'W', message.where_.as_deref(), "error context")?;
    write_optional_field(&mut body, b's', message.schema.as_deref(), "error schema")?;
    write_optional_field(&mut body, b't', message.table.as_deref(), "error table")?;
    write_optional_field(&mut body, b'c', message.column.as_deref(), "error column")?;
    write_optional_field(
        &mut body,
        b'd',
        message.data_type.as_deref(),
        "error data type",
    )?;
    write_optional_field(
        &mut body,
        b'n',
        message.constraint.as_deref(),
        "error constraint",
    )?;
    write_optional_field(&mut body, b'F', message.file.as_deref(), "error file")?;
    write_optional_i32_field(&mut body, b'L', message.line, "error line")?;
    write_optional_field(&mut body, b'R', message.routine.as_deref(), "error routine")?;
    body.write_byte(0);
    Writer::frame(tag, &body.into_inner())
}

fn encode_parameter_description(oids: &[u32]) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_i16(i16_len(oids.len(), "ParameterDescription parameter")?);
    for oid in oids {
        body.write_u32(*oid);
    }
    Writer::frame(b't', &body.into_inner())
}

fn encode_copy_response(tag: u8, response: &CopyResponse) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_byte(match response.overall_format {
        FormatCode::Text => 0,
        FormatCode::Binary => 1,
    });
    body.write_i16(i16_len(
        response.column_formats.len(),
        "CopyResponse column",
    )?);
    for format in &response.column_formats {
        body.write_format(*format);
    }
    Writer::frame(tag, &body.into_inner())
}

fn encode_copy_fail(message: &str) -> Result<Vec<u8>, PgWireError> {
    let mut body = Writer::new();
    body.write_cstring(message, "CopyFail message")?;
    Writer::frame(b'f', &body.into_inner())
}

fn encode_empty_body(tag: u8) -> Result<Vec<u8>, PgWireError> {
    Writer::frame(tag, &[])
}

fn write_field(
    body: &mut Writer,
    code: u8,
    value: &str,
    context: &'static str,
) -> Result<(), PgWireError> {
    body.write_byte(code);
    body.write_cstring(value, context)
}

fn write_optional_field(
    body: &mut Writer,
    code: u8,
    value: Option<&str>,
    context: &'static str,
) -> Result<(), PgWireError> {
    if let Some(value) = value {
        write_field(body, code, value, context)?;
    }
    Ok(())
}

fn write_optional_i32_field(
    body: &mut Writer,
    code: u8,
    value: Option<i32>,
    context: &'static str,
) -> Result<(), PgWireError> {
    if let Some(value) = value {
        write_field(body, code, &value.to_string(), context)?;
    }
    Ok(())
}
