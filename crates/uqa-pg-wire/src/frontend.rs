//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use crate::codec::{message_total_len, DecodeLen, Reader, MESSAGE_HEADER_LEN};
use crate::protocol::{
    DecodeOutcome, FormatCode, PgWireError, ProtocolVersion, CANCEL_REQUEST_CODE,
    GSSENC_REQUEST_CODE, PROTOCOL_VERSION_3_0, SSL_REQUEST_CODE,
};

pub const DEFAULT_MAX_MESSAGE_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupFrame {
    Startup(StartupMessage),
    CancelRequest { process_id: i32, secret_key: i32 },
    SSLRequest,
    GSSEncRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupMessage {
    pub version: ProtocolVersion,
    pub parameters: BTreeMap<String, String>,
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
    Password(Vec<u8>),
    CopyData(Vec<u8>),
    CopyDone,
    CopyFail(String),
    FunctionCall(Vec<u8>),
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
            let secret_key = reader.read_i32("cancel request secret key")?;
            reader.ensure_empty("cancel request")?;
            StartupFrame::CancelRequest {
                process_id,
                secret_key,
            }
        }
        PROTOCOL_VERSION_3_0 => StartupFrame::Startup(parse_startup_message(
            ProtocolVersion::from_raw(version_or_code),
            reader,
        )?),
        other => return Err(PgWireError::UnsupportedProtocolVersion(other)),
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
        parameters.insert(key, value);
    }
    Ok(StartupMessage {
        version,
        parameters,
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
        b'p' => FrontendMessage::Password(body.to_vec()),
        b'd' => FrontendMessage::CopyData(body.to_vec()),
        b'c' => {
            reader.ensure_empty("CopyDone")?;
            FrontendMessage::CopyDone
        }
        b'f' => FrontendMessage::CopyFail(parse_single_cstring(&mut reader, "CopyFail")?),
        b'F' => FrontendMessage::FunctionCall(body.to_vec()),
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
