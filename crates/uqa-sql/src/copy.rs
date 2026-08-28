//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` `COPY` statement envelopes and stream codecs.
//!
//! `COPY FROM STDIN` and `COPY TO STDOUT` carry their byte stream outside the
//! SQL statement. The regular [`crate::compile`] entry point therefore cannot
//! execute one by itself; callers use [`compile_copy`] to validate the SQL
//! envelope and pair it with the stream through the engine COPY API.

use std::collections::BTreeSet;

use pg_query::NodeEnum;

use crate::SQLError;

mod codec;

pub use codec::{decode_copy_input, encode_copy_result, encode_copy_result_with_engine};

/// COPY stream direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyDirection {
    From,
    To,
}

/// COPY data encoding implemented by the embedded stream API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    Text,
    Csv,
    Binary,
}

/// `PostgreSQL`'s three `HEADER` states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyHeader {
    False,
    True,
    Match,
}

/// Parsed COPY options after format-dependent defaults have been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyOptions {
    pub format: CopyFormat,
    pub delimiter: u8,
    pub null: String,
    pub header: CopyHeader,
    pub quote: u8,
    pub escape: u8,
}

/// The relation or query supplying COPY rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyTarget {
    Relation {
        name: String,
        qualifier: String,
        columns: Vec<String>,
    },
    Query(String),
}

/// Where `PostgreSQL` expects the COPY stream to be connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEndpoint {
    Stdio,
    File(String),
    Program(String),
}

/// Fully validated COPY statement envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStatement {
    pub direction: CopyDirection,
    pub target: CopyTarget,
    pub endpoint: CopyEndpoint,
    pub options: CopyOptions,
}

/// One decoded COPY field. `None` is SQL NULL; strings remain in `PostgreSQL`'s
/// textual input representation so the target column's normal input coercion
/// remains authoritative.
pub type CopyInputField = Option<String>;

/// Parse exactly one `PostgreSQL` COPY statement.
pub fn compile_copy(sql: &str) -> Result<CopyStatement, SQLError> {
    let parsed = pg_query::parse(sql)?;
    if parsed.protobuf.stmts.len() != 1 {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "COPY stream API accepts exactly one COPY statement".into(),
        });
    }
    let node = parsed.protobuf.stmts[0]
        .stmt
        .as_deref()
        .and_then(|node| node.node.as_ref())
        .ok_or_else(|| SQLError::Internal("parser returned an empty COPY statement".into()))?;
    let NodeEnum::CopyStmt(statement) = node else {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "COPY stream API requires a COPY statement".into(),
        });
    };
    let direction = if statement.is_from {
        CopyDirection::From
    } else {
        CopyDirection::To
    };
    if statement.where_clause.is_some() {
        return Err(SQLError::Unsupported(
            "COPY FROM WHERE is not implemented".into(),
        ));
    }
    let target = match (&statement.relation, &statement.query) {
        (Some(relation), None) => {
            if !relation.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "COPY across databases is not supported".into(),
                ));
            }
            let name = if relation.schemaname.is_empty() {
                quote_relation_component(&relation.relname)
            } else {
                format!(
                    "{}.{}",
                    quote_relation_component(&relation.schemaname),
                    quote_relation_component(&relation.relname)
                )
            };
            let columns = statement
                .attlist
                .iter()
                .map(copy_column_name)
                .collect::<Result<Vec<_>, _>>()?;
            CopyTarget::Relation {
                name,
                qualifier: relation.relname.clone(),
                columns,
            }
        }
        (None, Some(query)) if direction == CopyDirection::To => CopyTarget::Query(
            query
                .deparse()
                .map_err(|error| SQLError::Parse(error.to_string()))?,
        ),
        (None, Some(_)) => {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "COPY FROM does not accept a query".into(),
            });
        }
        _ => {
            return Err(SQLError::Internal(
                "COPY statement has neither one relation nor one query".into(),
            ));
        }
    };
    let endpoint = if statement.is_program {
        CopyEndpoint::Program(statement.filename.clone())
    } else if statement.filename.is_empty() {
        CopyEndpoint::Stdio
    } else {
        CopyEndpoint::File(statement.filename.clone())
    };
    let options = compile_copy_options(&statement.options)?;
    if direction == CopyDirection::To && options.header == CopyHeader::Match {
        return Err(copy_option_error(
            "COPY HEADER MATCH is only valid for COPY FROM",
        ));
    }
    Ok(CopyStatement {
        direction,
        target,
        endpoint,
        options,
    })
}

fn quote_relation_component(component: &str) -> String {
    crate::expr::quote_ident(component)
}

fn copy_column_name(node: &pg_query::Node) -> Result<String, SQLError> {
    match node.node.as_ref() {
        Some(NodeEnum::String(value)) => Ok(value.sval.clone()),
        other => Err(SQLError::Internal(format!(
            "COPY column list contains malformed node {other:?}"
        ))),
    }
}

fn compile_copy_options(nodes: &[pg_query::Node]) -> Result<CopyOptions, SQLError> {
    let raw = nodes
        .iter()
        .map(|node| {
            let Some(NodeEnum::DefElem(option)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "COPY option list contains a malformed node".into(),
                ));
            };
            Ok((
                option.defname.to_ascii_lowercase(),
                copy_option_value(option)?,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    let mut names = BTreeSet::new();
    for (name, _) in &raw {
        if !names.insert(name.clone()) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!("COPY option \"{name}\" specified more than once"),
            });
        }
    }
    let format = raw.iter().find(|(name, _)| name == "format").map_or(
        Ok(CopyFormat::Text),
        |(_, value)| match value.to_ascii_lowercase().as_str() {
            "text" => Ok(CopyFormat::Text),
            "csv" => Ok(CopyFormat::Csv),
            "binary" => Ok(CopyFormat::Binary),
            other => Err(copy_option_error(format!(
                "COPY format \"{other}\" not recognized"
            ))),
        },
    )?;
    let mut options = CopyOptions {
        format,
        delimiter: if format == CopyFormat::Csv {
            b','
        } else {
            b'\t'
        },
        null: if format == CopyFormat::Csv {
            String::new()
        } else {
            "\\N".into()
        },
        header: CopyHeader::False,
        quote: b'"',
        escape: b'"',
    };
    let quote_explicit = names.contains("quote");
    let escape_explicit = names.contains("escape");
    for (name, value) in raw {
        match name.as_str() {
            "format" => {}
            "delimiter" => options.delimiter = copy_single_byte_option("DELIMITER", &value)?,
            "null" => options.null = value,
            "header" => {
                options.header = if value.eq_ignore_ascii_case("match") {
                    CopyHeader::Match
                } else if copy_boolean_option("HEADER", &value)? {
                    CopyHeader::True
                } else {
                    CopyHeader::False
                };
            }
            "quote" => options.quote = copy_single_byte_option("QUOTE", &value)?,
            "escape" => options.escape = copy_single_byte_option("ESCAPE", &value)?,
            "encoding" if matches!(value.to_ascii_lowercase().as_str(), "utf8" | "utf-8") => {}
            "encoding" => {
                return Err(SQLError::Unsupported(format!(
                    "COPY ENCODING {value} is not implemented"
                )));
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "COPY option `{other}` is not implemented"
                )));
            }
        }
    }
    validate_copy_options(&options, quote_explicit, escape_explicit)?;
    Ok(options)
}

fn copy_option_value(option: &pg_query::protobuf::DefElem) -> Result<String, SQLError> {
    match option
        .arg
        .as_deref()
        .and_then(|argument| argument.node.as_ref())
    {
        None => Ok("true".into()),
        Some(NodeEnum::String(value)) => Ok(value.sval.clone()),
        Some(NodeEnum::Integer(value)) => Ok(value.ival.to_string()),
        Some(NodeEnum::Float(value)) => Ok(value.fval.clone()),
        Some(NodeEnum::Boolean(value)) => Ok(value.boolval.to_string()),
        other => Err(copy_option_error(format!(
            "COPY option \"{}\" requires a scalar value, got {other:?}",
            option.defname
        ))),
    }
}

fn copy_boolean_option(name: &str, value: &str) -> Result<bool, SQLError> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Ok(true),
        "false" | "off" | "no" | "0" => Ok(false),
        _ => Err(copy_option_error(format!(
            "COPY {name} requires a Boolean value"
        ))),
    }
}

fn copy_single_byte_option(name: &str, value: &str) -> Result<u8, SQLError> {
    if value.len() != 1 {
        return Err(copy_option_error(format!(
            "COPY {name} must be a single one-byte character"
        )));
    }
    let byte = value.as_bytes()[0];
    if matches!(byte, 0 | b'\n' | b'\r') {
        return Err(copy_option_error(format!(
            "COPY {name} cannot be newline or carriage return"
        )));
    }
    Ok(byte)
}

fn validate_copy_options(
    options: &CopyOptions,
    quote_explicit: bool,
    escape_explicit: bool,
) -> Result<(), SQLError> {
    if options.null.contains(['\n', '\r']) {
        return Err(copy_option_error(
            "COPY null representation cannot use newline or carriage return",
        ));
    }
    if options.format == CopyFormat::Text && options.delimiter == b'\\' {
        return Err(copy_option_error("COPY delimiter cannot be backslash"));
    }
    if options.format != CopyFormat::Csv && (quote_explicit || escape_explicit) {
        return Err(copy_option_error(
            "COPY quote or escape available only in CSV mode",
        ));
    }
    if options.format == CopyFormat::Binary && options.header != CopyHeader::False {
        return Err(copy_option_error(
            "COPY HEADER is not available in binary mode",
        ));
    }
    if options.format == CopyFormat::Csv && options.delimiter == options.quote {
        return Err(copy_option_error(
            "COPY delimiter and quote must be different",
        ));
    }
    if options.null.as_bytes().contains(&options.delimiter) {
        return Err(copy_option_error(
            "COPY delimiter character must not appear in the NULL specification",
        ));
    }
    if options.format == CopyFormat::Csv && options.null.as_bytes().contains(&options.quote) {
        return Err(copy_option_error(
            "CSV quote character must not appear in the NULL specification",
        ));
    }
    Ok(())
}

fn copy_option_error(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_parser_preserves_relation_query_and_format_contracts() {
        let from = compile_copy(
            "COPY app.items (bucket, payload) FROM STDIN WITH (FORMAT csv, HEADER MATCH)",
        )
        .unwrap();
        assert_eq!(from.direction, CopyDirection::From);
        assert_eq!(from.endpoint, CopyEndpoint::Stdio);
        assert_eq!(from.options.format, CopyFormat::Csv);
        assert_eq!(from.options.header, CopyHeader::Match);
        assert_eq!(
            from.target,
            CopyTarget::Relation {
                name: "app.items".into(),
                qualifier: "items".into(),
                columns: vec!["bucket".into(), "payload".into()]
            }
        );

        let to = compile_copy("COPY (SELECT * FROM ONLY items ORDER BY id) TO STDOUT").unwrap();
        assert_eq!(to.direction, CopyDirection::To);
        let CopyTarget::Query(query) = to.target else {
            panic!("expected COPY query target");
        };
        assert!(query.contains("ONLY items"));

        let quoted =
            compile_copy("COPY \"copy.schema\".\"Odd.Table\" (\"Odd Column\") FROM STDIN").unwrap();
        assert_eq!(
            quoted.target,
            CopyTarget::Relation {
                name: "\"copy.schema\".\"Odd.Table\"".into(),
                qualifier: "Odd.Table".into(),
                columns: vec!["Odd Column".into()],
            }
        );

        for invalid in [
            r"COPY items FROM STDIN WITH (FORMAT text, DELIMITER E'\\')",
            "COPY items FROM STDIN WITH (FORMAT text, QUOTE '\"')",
            "COPY items FROM STDIN WITH (FORMAT binary, ESCAPE '\"')",
            "COPY items FROM STDIN WITH (FORMAT binary, HEADER)",
            "COPY items FROM STDIN WITH (DELIMITER '|', NULL 'a|b')",
            "COPY items FROM STDIN WITH (FORMAT csv, NULL 'a\"b')",
        ] {
            assert_eq!(compile_copy(invalid).unwrap_err().sqlstate(), Some("22023"));
        }
    }

    #[test]
    fn text_codec_distinguishes_null_escapes_and_end_marker() {
        let options = compile_copy("COPY t FROM STDIN").unwrap().options;
        let columns = vec!["a".into(), "b".into()];
        let rows = decode_copy_input(
            b"\\N\tempty\\tvalue\n\\\\N\tline\\nvalue\n\\.\nignored\tx\n",
            &options,
            &columns,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], None);
        assert_eq!(rows[0][1].as_deref(), Some("empty\tvalue"));
        assert_eq!(rows[1][0].as_deref(), Some("\\N"));
        assert_eq!(rows[1][1].as_deref(), Some("line\nvalue"));

        let custom = compile_copy("COPY t FROM STDIN WITH (DELIMITER '|')")
            .unwrap()
            .options;
        let escaped_delimiter =
            decode_copy_input(b"left\\|right|tail\n", &custom, &columns).unwrap();
        assert_eq!(
            escaped_delimiter[0],
            vec![Some("left|right".into()), Some("tail".into())]
        );
        assert_eq!(
            decode_copy_input(b"\\000\tvalue\n", &options, &columns)
                .unwrap_err()
                .sqlstate(),
            Some("22021")
        );

        let with_header = compile_copy("COPY t FROM STDIN WITH (HEADER MATCH)")
            .unwrap()
            .options;
        assert_eq!(
            decode_copy_input(b"a\tb\n1\t2\n", &with_header, &columns).unwrap(),
            vec![vec![Some("1".into()), Some("2".into())]]
        );
    }

    #[test]
    fn csv_codec_preserves_quoted_empty_and_embedded_newline() {
        let options = compile_copy("COPY t FROM STDIN WITH (FORMAT csv)")
            .unwrap()
            .options;
        let columns = vec!["a".into(), "b".into()];
        let rows =
            decode_copy_input(b",\"\"\n\"a,b\",\"line\nvalue\"\n", &options, &columns).unwrap();
        assert_eq!(rows[0], vec![None, Some(String::new())]);
        assert_eq!(rows[1][0].as_deref(), Some("a,b"));
        assert_eq!(rows[1][1].as_deref(), Some("line\nvalue"));

        let permissive = decode_copy_input(b"\"a\"b,c\n\"a\" ,b\n", &options, &columns).unwrap();
        assert_eq!(permissive[0], vec![Some("ab".into()), Some("c".into())]);
        assert_eq!(permissive[1], vec![Some("a ".into()), Some("b".into())]);
    }
}
