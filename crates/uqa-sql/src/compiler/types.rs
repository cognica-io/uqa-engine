//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column type and foreign-key constraint lowering helpers.

use pg_query::protobuf::Node;
use pg_query::NodeEnum;

use crate::ast::{ColumnType, RangeSubtype};
use crate::error::{Result, SQLError};

use super::tree::extract_string;

/// Parser-normalized type identity used by `PostgreSQL`'s `regtype` and `regprocedure` input functions. Components retain the parser's distinction between aliases such as unquoted `integer` (normalized to `pg_catalog.int4`) and a quoted type named `"integer"`; type modifiers are intentionally omitted because these aliases identify a base catalog type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRegtypeName {
    pub names: Vec<String>,
    pub array_dimensions: usize,
}

/// Parser-normalized routine name and optional exact input-type signature used by `PostgreSQL`'s `regproc` and `regprocedure` input functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRegprocedureName {
    pub names: Vec<String>,
    pub argument_types: Option<Vec<ParsedRegtypeName>>,
}

const POSTGRES_IDENTIFIER_MAX_BYTES: usize = 63;
const POSTGRES_FUNCTION_MAX_ARGUMENTS: usize = 100;

fn scanner_isspace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn truncate_postgres_identifier(mut identifier: String) -> String {
    if identifier.len() <= POSTGRES_IDENTIFIER_MAX_BYTES {
        return identifier;
    }
    let mut end = POSTGRES_IDENTIFIER_MAX_BYTES;
    while !identifier.is_char_boundary(end) {
        end -= 1;
    }
    identifier.truncate(end);
    identifier
}

/// Parse the dotted identifier strings consumed by `PostgreSQL`'s `reg*` input functions. This follows `SplitIdentifierString`: surrounding component whitespace is ignored, quoted components collapse doubled quotes without case folding, unquoted components extend to a dot or whitespace and use ASCII case folding, and every component is clipped to `PostgreSQL`'s 63-byte identifier limit.
#[must_use]
pub fn parse_regobject_name(input: &str) -> Option<Vec<String>> {
    let bytes = input.as_bytes();
    let mut offset = 0usize;
    while bytes.get(offset).is_some_and(|byte| scanner_isspace(*byte)) {
        offset += 1;
    }
    if offset == bytes.len() {
        return None;
    }

    let mut names = Vec::new();
    loop {
        let component = if bytes[offset] == b'"' {
            offset += 1;
            let mut quoted = String::new();
            loop {
                let relative = bytes[offset..].iter().position(|byte| *byte == b'"')?;
                let quote = offset + relative;
                quoted.push_str(&input[offset..quote]);
                offset = quote + 1;
                if bytes.get(offset) == Some(&b'"') {
                    quoted.push('"');
                    offset += 1;
                    continue;
                }
                break;
            }
            quoted
        } else {
            let start = offset;
            while bytes
                .get(offset)
                .is_some_and(|byte| *byte != b'.' && !scanner_isspace(*byte))
            {
                offset += 1;
            }
            if offset == start {
                return None;
            }
            input[start..offset].to_ascii_lowercase()
        };
        names.push(truncate_postgres_identifier(component));

        while bytes.get(offset).is_some_and(|byte| scanner_isspace(*byte)) {
            offset += 1;
        }
        match bytes.get(offset) {
            None => return Some(names),
            Some(b'.') => {
                offset += 1;
                while bytes.get(offset).is_some_and(|byte| scanner_isspace(*byte)) {
                    offset += 1;
                }
                if offset == bytes.len() {
                    return None;
                }
            }
            Some(_) => return None,
        }
    }
}

/// Parse exactly one `PostgreSQL` type-name string without accepting adjacent SQL expressions. `None` is the soft-failure shape used for inputs such as an empty string or `SETOF integer`; lexical and type-name syntax errors are retained for the caller.
pub fn parse_regtype_name(input: &str) -> Result<Option<ParsedRegtypeName>> {
    if input.bytes().all(scanner_isspace) {
        return Ok(None);
    }
    let parsed = pg_query::parse_with_mode(input, pg_query::ParseMode::TypeName)?;
    let [raw] = parsed.protobuf.stmts.as_slice() else {
        return Ok(None);
    };
    let Some(NodeEnum::List(names)) = raw.stmt.as_ref().and_then(|node| node.node.as_ref()) else {
        return Ok(None);
    };
    let names = names
        .items
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if names.is_empty() {
        return Ok(None);
    }
    let scanned = pg_query::scan(input)?;
    let tokens = scanned
        .tokens
        .iter()
        .filter_map(|token| pg_query::protobuf::Token::try_from(token.token).ok())
        .collect::<Vec<_>>();
    if tokens.contains(&pg_query::protobuf::Token::Setof) {
        return Ok(None);
    }
    let bracket_dimensions = tokens
        .iter()
        .filter(|token| **token == pg_query::protobuf::Token::Ascii91)
        .count();
    let array_dimensions = bracket_dimensions.max(usize::from(
        tokens.contains(&pg_query::protobuf::Token::Array),
    ));
    Ok(Some(ParsedRegtypeName {
        names,
        array_dimensions,
    }))
}

/// Parse one routine-name string with either an omitted signature (`regproc`) or an exact signature (`regprocedure`). The caller owns the soft-error and cross-database policy because the two SQL input functions intentionally differ from ordinary DDL.
pub fn parse_regprocedure_name(input: &str) -> Result<Option<ParsedRegprocedureName>> {
    let mut in_quote = false;
    let left_parenthesis = input.bytes().enumerate().find_map(|(offset, byte)| {
        if byte == b'"' {
            in_quote = !in_quote;
            None
        } else if byte == b'(' && !in_quote {
            Some(offset)
        } else {
            None
        }
    });
    let Some(left_parenthesis) = left_parenthesis else {
        return Ok(
            parse_regobject_name(input).map(|names| ParsedRegprocedureName {
                names,
                argument_types: None,
            }),
        );
    };
    let Some(names) = parse_regobject_name(&input[..left_parenthesis]) else {
        return Ok(None);
    };

    let bytes = input.as_bytes();
    let mut end = bytes.len();
    while end > left_parenthesis + 1 && scanner_isspace(bytes[end - 1]) {
        end -= 1;
    }
    if end <= left_parenthesis + 1 || bytes[end - 1] != b')' {
        return Err(SQLError::Parse(format!(
            "expected a right parenthesis in routine identity \"{input}\""
        )));
    }
    let arguments = &input[left_parenthesis + 1..end - 1];
    let argument_bytes = arguments.as_bytes();
    let mut argument_types = Vec::new();
    let mut offset = 0usize;
    let mut had_comma = false;
    loop {
        while argument_bytes
            .get(offset)
            .is_some_and(|byte| scanner_isspace(*byte))
        {
            offset += 1;
        }
        if offset == argument_bytes.len() {
            if had_comma {
                return Err(SQLError::Parse(format!(
                    "expected a type name in routine identity \"{input}\""
                )));
            }
            break;
        }

        let start = offset;
        let mut quoted = false;
        let mut nesting = 0i32;
        while let Some(byte) = argument_bytes.get(offset).copied() {
            if byte == b'"' {
                quoted = !quoted;
            } else if byte == b',' && !quoted && nesting == 0 {
                break;
            } else if !quoted {
                match byte {
                    b'(' | b'[' => nesting += 1,
                    b')' | b']' => nesting -= 1,
                    _ => {}
                }
            }
            offset += 1;
        }
        if quoted || nesting != 0 {
            return Err(SQLError::Parse(format!(
                "improper type name in routine identity \"{input}\""
            )));
        }
        let mut type_end = offset;
        while type_end > start && scanner_isspace(argument_bytes[type_end - 1]) {
            type_end -= 1;
        }
        let Some(type_name) = parse_regtype_name(&arguments[start..type_end])? else {
            return Ok(None);
        };
        if argument_types.len() == POSTGRES_FUNCTION_MAX_ARGUMENTS {
            return Err(SQLError::Parse(format!(
                "too many arguments in routine identity \"{input}\""
            )));
        }
        argument_types.push(type_name);
        had_comma = argument_bytes.get(offset) == Some(&b',');
        if had_comma {
            offset += 1;
        }
    }

    Ok(Some(ParsedRegprocedureName {
        names,
        argument_types: Some(argument_types),
    }))
}

pub(super) fn compile_foreign_key_action(raw: &str) -> Result<crate::ast::ForeignKeyAction> {
    use crate::ast::ForeignKeyAction;
    match raw.as_bytes().first().copied() {
        None | Some(0) | Some(b'a') => Ok(ForeignKeyAction::NoAction),
        Some(b'r') => Ok(ForeignKeyAction::Restrict),
        Some(b'c') => Ok(ForeignKeyAction::Cascade),
        Some(b'n') => Ok(ForeignKeyAction::SetNull),
        Some(b'd') => Ok(ForeignKeyAction::SetDefault),
        Some(other) => Err(SQLError::Unsupported(format!(
            "unsupported FOREIGN KEY action byte {other:?}"
        ))),
    }
}

pub(super) fn compile_foreign_key_match(raw: &str) -> Result<crate::ast::ForeignKeyMatch> {
    use crate::ast::ForeignKeyMatch;
    match raw.as_bytes().first().copied() {
        None | Some(0) | Some(b's') => Ok(ForeignKeyMatch::Simple),
        Some(b'f') => Ok(ForeignKeyMatch::Full),
        Some(b'p') => Err(SQLError::Unsupported(
            "FOREIGN KEY MATCH PARTIAL is not implemented by PostgreSQL".into(),
        )),
        Some(other) => Err(SQLError::Unsupported(format!(
            "unsupported FOREIGN KEY match byte {other:?}"
        ))),
    }
}

pub(super) fn validate_foreign_key_set_columns(
    local_columns: &[String],
    set_columns: &[String],
    raw_delete_action: &str,
) -> Result<()> {
    if set_columns.is_empty() {
        return Ok(());
    }
    let action = compile_foreign_key_action(raw_delete_action)?;
    if !matches!(
        action,
        crate::ast::ForeignKeyAction::SetNull | crate::ast::ForeignKeyAction::SetDefault
    ) {
        return Err(SQLError::Unsupported(
            "FOREIGN KEY column lists are only valid for ON DELETE SET NULL/DEFAULT".into(),
        ));
    }
    for col in set_columns {
        if !local_columns.iter().any(|local| local == col) {
            return Err(SQLError::Unsupported(format!(
                "FOREIGN KEY SET column `{col}` is not part of the local key"
            )));
        }
    }
    Ok(())
}

pub(super) fn raw_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<Option<String>> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Ok(None);
    };
    let names = type_name
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    Ok(names.last().map(|name| name.to_lowercase()))
}

pub(super) fn compile_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnType> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Err(SQLError::Internal(format!(
            "column `{}` has no type",
            col.colname
        )));
    };
    compile_pg_type_name(type_name, &col.colname)
}

#[expect(
    clippy::too_many_lines,
    reason = "ordered PostgreSQL lowering preserves syntax and error precedence"
)]
pub(super) fn compile_pg_type_name(
    type_name: &pg_query::protobuf::TypeName,
    column_name: &str,
) -> Result<ColumnType> {
    let names = type_name
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    let raw = names
        .last()
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "type name for `{column_name}` has no name components"
            ))
        })?
        .to_lowercase();
    let base = match raw.as_str() {
        "smallint" | "int2" | "smallserial" | "serial2" => Ok(ColumnType::SmallInteger),
        "int" | "int4" | "integer" | "serial" | "serial4" => Ok(ColumnType::Integer),
        "bigint" | "int8" | "bigserial" | "serial8" => Ok(ColumnType::BigInteger),
        "oid" => Ok(ColumnType::Oid),
        "xid" => Ok(ColumnType::Xid),
        "text" => Ok(ColumnType::Text),
        "name" => Ok(ColumnType::Name),
        "uuid" => Ok(ColumnType::Uuid),
        "varchar" | "character varying" => {
            if type_name.typmods.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "CHARACTER VARYING accepts at most one length modifier, got {}",
                    type_name.typmods.len()
                )));
            }
            let length = type_name
                .typmods
                .first()
                .map(expect_positive_character_length)
                .transpose()?;
            Ok(ColumnType::Varchar(length))
        }
        "character" | "char" | "bpchar" => {
            if type_name.typmods.len() > 1 {
                return Err(SQLError::TypeMismatch(format!(
                    "CHARACTER accepts at most one length modifier, got {}",
                    type_name.typmods.len()
                )));
            }
            let length = type_name
                .typmods
                .first()
                .map(expect_positive_character_length)
                .transpose()?
                .unwrap_or(1);
            Ok(ColumnType::Character(length))
        }
        "bool" | "boolean" => Ok(ColumnType::Boolean),
        "real" | "float4" => Ok(ColumnType::Real),
        "float8" | "double" | "double precision" => Ok(ColumnType::DoublePrecision),
        "numeric" | "decimal" => {
            if type_name.typmods.len() > 2 {
                return Err(SQLError::TypeMismatch(format!(
                    "NUMERIC accepts at most precision and scale, got {} modifiers",
                    type_name.typmods.len()
                )));
            }
            let mut typmods_iter = type_name.typmods.iter();
            let precision = typmods_iter
                .next()
                .map(|n| {
                    let value = expect_integer_const(n)?;
                    if !(1..=1000).contains(&value) {
                        return Err(SQLError::TypeMismatch(format!(
                            "NUMERIC precision must be between 1 and 1000, got {value}"
                        )));
                    }
                    Ok(value as u32)
                })
                .transpose()?;
            let scale = typmods_iter
                .next()
                .map(|n| {
                    let value = expect_integer_const(n)?;
                    if !(-1000..=1000).contains(&value) {
                        return Err(SQLError::TypeMismatch(format!(
                            "NUMERIC scale must be between -1000 and 1000, got {value}"
                        )));
                    }
                    Ok(value as i32)
                })
                .transpose()?;
            // PostgreSQL semantics: NUMERIC(precision) without an
            // explicit scale defaults to scale=0, rounding to integers.
            let scale = scale.or(precision.map(|_| 0));
            Ok(ColumnType::Numeric { precision, scale })
        }
        "date" => Ok(ColumnType::Date),
        "time" | "time without time zone" => Ok(ColumnType::Time),
        "timetz" | "time with time zone" => Ok(ColumnType::TimeTz),
        "timestamp" | "datetime" | "timestamp without time zone" => Ok(ColumnType::Timestamp),
        "timestamptz" | "timestamp with time zone" => Ok(ColumnType::TimestampTz),
        "interval" => Ok(ColumnType::Interval),
        "int4range" => Ok(ColumnType::Range(RangeSubtype::Integer)),
        "int8range" => Ok(ColumnType::Range(RangeSubtype::BigInteger)),
        "numrange" => Ok(ColumnType::Range(RangeSubtype::Numeric)),
        "daterange" => Ok(ColumnType::Range(RangeSubtype::Date)),
        "tsrange" => Ok(ColumnType::Range(RangeSubtype::Timestamp)),
        "tstzrange" => Ok(ColumnType::Range(RangeSubtype::TimestampTz)),
        "int4multirange" => Ok(ColumnType::Multirange(RangeSubtype::Integer)),
        "int8multirange" => Ok(ColumnType::Multirange(RangeSubtype::BigInteger)),
        "nummultirange" => Ok(ColumnType::Multirange(RangeSubtype::Numeric)),
        "datemultirange" => Ok(ColumnType::Multirange(RangeSubtype::Date)),
        "tsmultirange" => Ok(ColumnType::Multirange(RangeSubtype::Timestamp)),
        "tstzmultirange" => Ok(ColumnType::Multirange(RangeSubtype::TimestampTz)),
        "json" => Ok(ColumnType::Json),
        "jsonb" => Ok(ColumnType::JsonB),
        "bytea" => Ok(ColumnType::Bytea),
        "regproc" => Ok(ColumnType::Regproc),
        "regprocedure" => Ok(ColumnType::Regprocedure),
        "regclass" => Ok(ColumnType::Regclass),
        "regnamespace" => Ok(ColumnType::Regnamespace),
        "regrole" => Ok(ColumnType::Regrole),
        "regtype" => Ok(ColumnType::Regtype),
        "pg_node_tree" => Ok(ColumnType::PgNodeTree),
        "aclitem" => Ok(ColumnType::AclItem),
        "int2vector" => Ok(ColumnType::Int2Vector),
        "oidvector" => Ok(ColumnType::OidVector),
        "vector" => {
            // VECTOR(N): the dimension is the only typmod argument.
            let [arg] = type_name.typmods.as_slice() else {
                return Err(SQLError::Unsupported(
                    "VECTOR requires exactly one dimension".into(),
                ));
            };
            let raw_dim = expect_integer_const(arg)?;
            let dim = u32::try_from(raw_dim).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "VECTOR dimension must be between 1 and {}, got {raw_dim}",
                    u32::MAX
                ))
            })?;
            if dim == 0 {
                return Err(SQLError::TypeMismatch(
                    "VECTOR dimension must be greater than zero".into(),
                ));
            }
            Ok(ColumnType::Vector(dim))
        }
        "tensor" => {
            // TENSOR(N): an array of N-dimensional vectors.
            let [arg] = type_name.typmods.as_slice() else {
                return Err(SQLError::Unsupported(
                    "TENSOR requires exactly one dimension".into(),
                ));
            };
            let raw_dim = expect_integer_const(arg)?;
            let dim = u32::try_from(raw_dim).map_err(|_| {
                SQLError::TypeMismatch(format!(
                    "TENSOR dimension must be between 1 and {}, got {raw_dim}",
                    u32::MAX
                ))
            })?;
            if dim == 0 {
                return Err(SQLError::TypeMismatch(
                    "TENSOR dimension must be greater than zero".into(),
                ));
            }
            Ok(ColumnType::Tensor(dim))
        }
        other => Err(SQLError::Unsupported(format!(
            "column `{column_name}` type `{other}` is not supported"
        ))),
    }?;
    Ok(type_name
        .array_bounds
        .iter()
        .fold(base, |element, _| ColumnType::Array(Box::new(element))))
}

fn expect_positive_character_length(node: &Node) -> Result<u32> {
    let length = expect_integer_const(node)?;
    u32::try_from(length)
        .ok()
        .filter(|length| *length > 0)
        .ok_or_else(|| {
            SQLError::TypeMismatch(format!(
                "character length must be greater than zero, got {length}"
            ))
        })
}

fn expect_integer_const(node: &Node) -> Result<i64> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing const node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => Ok(i64::from(i.ival)),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => {
                f.fval.parse::<i64>().map_err(|_| {
                    SQLError::TypeMismatch(format!(
                        "type modifier must be an integer, got `{}`",
                        f.fval
                    ))
                })
            }
            other => Err(SQLError::Internal(format!(
                "expected integer constant, got {other:?}"
            ))),
        },
        _ => Err(SQLError::Internal(format!(
            "expected A_Const, got {inner:?}"
        ))),
    }
}
