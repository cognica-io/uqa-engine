//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use crate::codec::{message_total_len, DecodeLen, Reader, MESSAGE_HEADER_LEN};
use crate::protocol::{
    resolve_format_code, resolve_format_codes, CancelKey, DecodeOutcome, FormatCode, PgWireError,
    ProtocolVersion, CANCEL_REQUEST_CODE, GSSENC_REQUEST_CODE, SSL_REQUEST_CODE,
};

pub const DEFAULT_MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFrame {
    Startup(StartupMessage),
    CancelRequest {
        process_id: i32,
        secret_key: CancelKey,
    },
    SSLRequest,
    GSSEncRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupMessage {
    pub version: ProtocolVersion,
    pub parameters: BTreeMap<String, String>,
    /// Startup parameter pairs in wire order, including duplicate names.
    pub parameter_pairs: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupNegotiation {
    pub requested_version: ProtocolVersion,
    pub negotiated_version: ProtocolVersion,
    pub unrecognized_options: Vec<String>,
}

impl StartupNegotiation {
    #[must_use]
    pub fn requires_response(&self) -> bool {
        self.requested_version != self.negotiated_version || !self.unrecognized_options.is_empty()
    }

    #[must_use]
    pub fn response(&self) -> Option<crate::backend::BackendMessage> {
        self.requires_response().then(
            || crate::backend::BackendMessage::NegotiateProtocolVersion {
                newest_protocol_version: self.negotiated_version,
                unrecognized_options: self.unrecognized_options.clone(),
            },
        )
    }
}

impl StartupMessage {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.parameters.get(key).map(String::as_str)
    }

    pub fn user(&self) -> Option<&str> {
        self.get("user")
    }

    pub fn database(&self) -> Option<&str> {
        self.get("database")
    }

    pub fn application_name(&self) -> Option<&str> {
        self.get("application_name")
    }

    /// Negotiate a `PostgreSQL` 3.x minor version and report every `_pq_.`
    /// startup option the embedding server has not implemented.
    pub fn negotiate(
        &self,
        supported_protocol_options: &[&str],
    ) -> Result<StartupNegotiation, PgWireError> {
        self.negotiate_with_max(ProtocolVersion::LATEST, supported_protocol_options)
    }

    /// Negotiate against the newest protocol version implemented by the
    /// embedding server.
    pub fn negotiate_with_max(
        &self,
        newest_supported: ProtocolVersion,
        supported_protocol_options: &[&str],
    ) -> Result<StartupNegotiation, PgWireError> {
        let negotiated_version = self.version.negotiate_with_max(newest_supported)?;
        let unrecognized_options = self
            .parameter_pairs
            .iter()
            .map(|(name, _)| name)
            .filter(|name| {
                name.starts_with("_pq_.") && !supported_protocol_options.contains(&name.as_str())
            })
            .cloned()
            .collect();
        Ok(StartupNegotiation {
            requested_version: self.version,
            negotiated_version,
            unrecognized_options,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendMessage {
    Query(String),
    Parse(Parse),
    Bind(Bind),
    Describe(DescribeTarget),
    Execute(Execute),
    Close(CloseTarget),
    Flush,
    Sync,
    Terminate,
    Password(PasswordMessage),
    CopyData(Vec<u8>),
    CopyDone,
    CopyFail(String),
    FunctionCall(FunctionCall),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse {
    pub statement: String,
    pub query: String,
    pub parameter_type_oids: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    pub portal: String,
    pub statement: String,
    pub parameter_formats: Vec<FormatCode>,
    pub parameters: Vec<Option<Vec<u8>>>,
    pub result_formats: Vec<FormatCode>,
}

impl Bind {
    /// Expand `PostgreSQL`'s zero, one, or one-per-parameter format-code forms.
    pub fn resolved_parameter_formats(&self) -> Result<Vec<FormatCode>, PgWireError> {
        resolve_format_codes(
            &self.parameter_formats,
            self.parameters.len(),
            |format_count, parameter_count| PgWireError::ParameterFormatCountMismatch {
                format_count,
                parameter_count,
            },
        )
    }

    /// Resolve the wire format for one bound parameter.
    pub fn parameter_format(&self, index: usize) -> Result<FormatCode, PgWireError> {
        resolve_format_code(
            &self.parameter_formats,
            self.parameters.len(),
            index,
            "Bind parameter",
            |format_count, parameter_count| PgWireError::ParameterFormatCountMismatch {
                format_count,
                parameter_count,
            },
        )
    }

    /// Expand `PostgreSQL`'s zero, one, or one-per-column result format forms.
    pub fn resolved_result_formats(
        &self,
        column_count: usize,
    ) -> Result<Vec<FormatCode>, PgWireError> {
        resolve_format_codes(
            &self.result_formats,
            column_count,
            |format_count, column_count| PgWireError::ResultFormatCountMismatch {
                format_count,
                column_count,
            },
        )
    }

    /// Resolve the requested wire format for one result column.
    pub fn result_format(
        &self,
        index: usize,
        column_count: usize,
    ) -> Result<FormatCode, PgWireError> {
        resolve_format_code(
            &self.result_formats,
            column_count,
            index,
            "Bind result column",
            |format_count, column_count| PgWireError::ResultFormatCountMismatch {
                format_count,
                column_count,
            },
        )
    }
}

/// The body of the context-dependent frontend message tagged `p`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordMessage(Vec<u8>);

impl PasswordMessage {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl AsRef<[u8]> for PasswordMessage {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescribeTarget {
    Statement(String),
    Portal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Execute {
    pub portal: String,
    pub max_rows: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseTarget {
    Statement(String),
    Portal(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionCall {
    pub function_oid: u32,
    pub argument_formats: Vec<FormatCode>,
    pub arguments: Vec<Option<Vec<u8>>>,
    pub result_format: FormatCode,
}

impl FunctionCall {
    /// Expand `PostgreSQL`'s zero, one, or one-per-argument format-code forms.
    pub fn resolved_argument_formats(&self) -> Result<Vec<FormatCode>, PgWireError> {
        resolve_format_codes(
            &self.argument_formats,
            self.arguments.len(),
            |format_count, argument_count| PgWireError::FunctionArgumentFormatCountMismatch {
                format_count,
                argument_count,
            },
        )
    }

    /// Resolve the wire format for one function-call argument.
    pub fn argument_format(&self, index: usize) -> Result<FormatCode, PgWireError> {
        resolve_format_code(
            &self.argument_formats,
            self.arguments.len(),
            index,
            "FunctionCall argument",
            |format_count, argument_count| PgWireError::FunctionArgumentFormatCountMismatch {
                format_count,
                argument_count,
            },
        )
    }
}

pub fn decode_startup(input: &[u8]) -> DecodeOutcome<StartupFrame> {
    decode_startup_with_max(input, DEFAULT_MAX_MESSAGE_LEN)
}

pub fn decode_startup_with_max(input: &[u8], max_len: usize) -> DecodeOutcome<StartupFrame> {
    let total = match message_total_len(input, false, max_len) {
        DecodeLen::Complete(total) => total,
        DecodeLen::Incomplete => return Ok(None),
        DecodeLen::Error(error) => return Err(error),
    };

    let body = &input[4..total];
    let mut reader = Reader::new(body);
    let version_or_code = reader.read_i32("startup version")?;
    let frame = match version_or_code {
        SSL_REQUEST_CODE => {
            reader.ensure_empty("SSL request")?;
            StartupFrame::SSLRequest
        }
        GSSENC_REQUEST_CODE => {
            reader.ensure_empty("GSSENC request")?;
            StartupFrame::GSSEncRequest
        }
        CANCEL_REQUEST_CODE => {
            let process_id = reader.read_i32("cancel request process id")?;
            let key_length = reader.remaining();
            let secret_key = CancelKey::new(
                reader
                    .read_exact(key_length, "cancel request secret key")?
                    .to_vec(),
            )?;
            reader.ensure_empty("cancel request")?;
            StartupFrame::CancelRequest {
                process_id,
                secret_key,
            }
        }
        other => {
            let version = ProtocolVersion::from_raw(other);
            version.negotiate()?;
            StartupFrame::Startup(parse_startup_message(version, reader)?)
        }
    };
    Ok(Some((frame, total)))
}

pub fn decode_frontend(input: &[u8]) -> DecodeOutcome<FrontendMessage> {
    decode_frontend_with_max(input, DEFAULT_MAX_MESSAGE_LEN)
}

pub fn decode_frontend_with_max(input: &[u8], max_len: usize) -> DecodeOutcome<FrontendMessage> {
    let total = match message_total_len(input, true, max_len) {
        DecodeLen::Complete(total) => total,
        DecodeLen::Incomplete => return Ok(None),
        DecodeLen::Error(error) => return Err(error),
    };

    let tag = input[0];
    let body = &input[MESSAGE_HEADER_LEN..total];
    let message = parse_frontend_message(tag, body)?;
    Ok(Some((message, total)))
}

fn parse_startup_message(
    version: ProtocolVersion,
    mut reader: Reader<'_>,
) -> Result<StartupMessage, PgWireError> {
    let mut parameters = BTreeMap::new();
    let mut parameter_pairs = Vec::new();
    loop {
        if reader.remaining() == 0 {
            return Err(PgWireError::MissingNul {
                context: "startup parameters",
            });
        }
        if reader.remaining() == 1 {
            let terminator = reader.read_byte("startup terminator")?;
            if terminator == 0 {
                break;
            }
            return Err(PgWireError::MissingNul {
                context: "startup parameters",
            });
        }

        let key = reader.read_cstring("startup parameter key")?;
        if key.is_empty() {
            reader.ensure_empty("startup parameters")?;
            break;
        }
        let value = reader.read_cstring("startup parameter value")?;
        parameter_pairs.push((key.clone(), value.clone()));
        parameters.insert(key, value);
    }
    Ok(StartupMessage {
        version,
        parameters,
        parameter_pairs,
    })
}

fn parse_frontend_message(tag: u8, body: &[u8]) -> Result<FrontendMessage, PgWireError> {
    let mut reader = Reader::new(body);
    let message = match tag {
        b'Q' => FrontendMessage::Query(parse_single_cstring(&mut reader, "Query")?),
        b'P' => FrontendMessage::Parse(parse_parse(&mut reader)?),
        b'B' => FrontendMessage::Bind(parse_bind(&mut reader)?),
        b'D' => FrontendMessage::Describe(parse_describe(&mut reader)?),
        b'E' => FrontendMessage::Execute(parse_execute(&mut reader)?),
        b'C' => FrontendMessage::Close(parse_close(&mut reader)?),
        b'H' => {
            reader.ensure_empty("Flush")?;
            FrontendMessage::Flush
        }
        b'S' => {
            reader.ensure_empty("Sync")?;
            FrontendMessage::Sync
        }
        b'X' => {
            reader.ensure_empty("Terminate")?;
            FrontendMessage::Terminate
        }
        b'p' => FrontendMessage::Password(PasswordMessage::new(body)),
        b'd' => FrontendMessage::CopyData(body.to_vec()),
        b'c' => {
            reader.ensure_empty("CopyDone")?;
            FrontendMessage::CopyDone
        }
        b'f' => FrontendMessage::CopyFail(parse_single_cstring(&mut reader, "CopyFail")?),
        b'F' => FrontendMessage::FunctionCall(parse_function_call(&mut reader)?),
        other => return Err(PgWireError::UnknownFrontendTag(other)),
    };
    Ok(message)
}

fn parse_parse(reader: &mut Reader<'_>) -> Result<Parse, PgWireError> {
    let statement = reader.read_cstring("Parse statement name")?;
    let query = reader.read_cstring("Parse query")?;
    let count = read_count(reader, "Parse parameter type count")?;
    let mut parameter_type_oids = Vec::with_capacity(count);
    for _ in 0..count {
        parameter_type_oids.push(reader.read_u32("Parse parameter type oid")?);
    }
    reader.ensure_empty("Parse")?;
    Ok(Parse {
        statement,
        query,
        parameter_type_oids,
    })
}

fn parse_bind(reader: &mut Reader<'_>) -> Result<Bind, PgWireError> {
    let portal = reader.read_cstring("Bind portal name")?;
    let statement = reader.read_cstring("Bind statement name")?;
    let parameter_format_count = read_count(reader, "Bind parameter format count")?;
    let mut parameter_formats = Vec::with_capacity(parameter_format_count);
    for _ in 0..parameter_format_count {
        parameter_formats.push(FormatCode::from_i16(
            reader.read_i16("Bind parameter format code")?,
        )?);
    }

    let parameter_count = read_count(reader, "Bind parameter count")?;
    if parameter_format_count > 1 && parameter_format_count != parameter_count {
        return Err(PgWireError::ParameterFormatCountMismatch {
            format_count: parameter_format_count,
            parameter_count,
        });
    }
    let mut parameters = Vec::with_capacity(parameter_count);
    for _ in 0..parameter_count {
        let value = match reader.read_len_i32("Bind parameter value length")? {
            Some(length) => Some(reader.read_exact(length, "Bind parameter value")?.to_vec()),
            None => None,
        };
        parameters.push(value);
    }

    let result_format_count = read_count(reader, "Bind result format count")?;
    let mut result_formats = Vec::with_capacity(result_format_count);
    for _ in 0..result_format_count {
        result_formats.push(FormatCode::from_i16(
            reader.read_i16("Bind result format code")?,
        )?);
    }
    reader.ensure_empty("Bind")?;
    Ok(Bind {
        portal,
        statement,
        parameter_formats,
        parameters,
        result_formats,
    })
}

fn parse_describe(reader: &mut Reader<'_>) -> Result<DescribeTarget, PgWireError> {
    let target = reader.read_byte("Describe target type")?;
    let name = reader.read_cstring("Describe target name")?;
    reader.ensure_empty("Describe")?;
    match target {
        b'S' => Ok(DescribeTarget::Statement(name)),
        b'P' => Ok(DescribeTarget::Portal(name)),
        other => Err(PgWireError::UnknownFrontendTag(other)),
    }
}

fn parse_execute(reader: &mut Reader<'_>) -> Result<Execute, PgWireError> {
    let portal = reader.read_cstring("Execute portal name")?;
    let max_rows = reader.read_i32("Execute max rows")?;
    if max_rows < 0 {
        return Err(PgWireError::NegativeValue {
            context: "Execute max rows",
        });
    }
    reader.ensure_empty("Execute")?;
    Ok(Execute { portal, max_rows })
}

fn parse_close(reader: &mut Reader<'_>) -> Result<CloseTarget, PgWireError> {
    let target = reader.read_byte("Close target type")?;
    let name = reader.read_cstring("Close target name")?;
    reader.ensure_empty("Close")?;
    match target {
        b'S' => Ok(CloseTarget::Statement(name)),
        b'P' => Ok(CloseTarget::Portal(name)),
        other => Err(PgWireError::UnknownFrontendTag(other)),
    }
}

fn parse_function_call(reader: &mut Reader<'_>) -> Result<FunctionCall, PgWireError> {
    let function_oid = reader.read_u32("FunctionCall function oid")?;
    let argument_format_count = read_count(reader, "FunctionCall argument format count")?;
    let mut argument_formats = Vec::with_capacity(argument_format_count);
    for _ in 0..argument_format_count {
        argument_formats.push(FormatCode::from_i16(
            reader.read_i16("FunctionCall argument format code")?,
        )?);
    }

    let argument_count = read_count(reader, "FunctionCall argument count")?;
    if argument_format_count > 1 && argument_format_count != argument_count {
        return Err(PgWireError::FunctionArgumentFormatCountMismatch {
            format_count: argument_format_count,
            argument_count,
        });
    }
    let mut arguments = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        let value = match reader.read_len_i32("FunctionCall argument value length")? {
            Some(length) => Some(
                reader
                    .read_exact(length, "FunctionCall argument value")?
                    .to_vec(),
            ),
            None => None,
        };
        arguments.push(value);
    }

    let result_format = FormatCode::from_i16(reader.read_i16("FunctionCall result format code")?)?;
    reader.ensure_empty("FunctionCall")?;
    Ok(FunctionCall {
        function_oid,
        argument_formats,
        arguments,
        result_format,
    })
}

fn parse_single_cstring(
    reader: &mut Reader<'_>,
    context: &'static str,
) -> Result<String, PgWireError> {
    let value = reader.read_cstring(context)?;
    reader.ensure_empty(context)?;
    Ok(value)
}

fn read_count(reader: &mut Reader<'_>, context: &'static str) -> Result<usize, PgWireError> {
    let count = reader.read_i16(context)?;
    if count < 0 {
        return Err(PgWireError::NegativeValue { context });
    }
    Ok(count as usize)
}
