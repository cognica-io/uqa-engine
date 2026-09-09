//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::sql::{format_postgres_text, postgres_result_type};
use uqa_engine::Engine;
use uqa_pg_wire::{BackendMessage, ErrorOrNotice, FieldDescription, FormatCode, NoticeSeverity};
use uqa_sql::{SQLError, SQLResult, SQLResultKind};

use crate::transport::Transport;
use crate::ServerError;

pub(crate) fn send_result(
    transport: &mut Transport,
    engine: &Engine,
    result: &SQLResult,
) -> Result<(), ServerError> {
    match result.kind {
        SQLResultKind::Rows => {
            let fields = result
                .columns
                .iter()
                .enumerate()
                .map(|(position, name)| {
                    let ty = result
                        .column_types
                        .get(position)
                        .and_then(Option::as_ref)
                        .ok_or_else(|| {
                            SQLError::Internal(format!(
                                "result column {position} has no bound SQL type"
                            ))
                        })?;
                    let metadata = postgres_result_type(ty);
                    Ok(FieldDescription {
                        name: name.clone(),
                        table_oid: 0,
                        column_attribute_number: 0,
                        type_oid: metadata.type_oid,
                        type_size: metadata.type_size,
                        type_modifier: metadata.type_modifier,
                        format: FormatCode::Text,
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            transport.send(&BackendMessage::RowDescription(fields))?;
            for row in 0..result.rows.len() {
                let values = (0..result.columns.len())
                    .map(|column| {
                        let value = result.value_at(row, column).ok_or_else(|| {
                            SQLError::Internal("result row has no positional value".into())
                        })?;
                        if matches!(value, Value::Null) {
                            return Ok(None);
                        }
                        let ty = result.column_types[column].as_ref().expect("bound above");
                        format_postgres_text(value, ty, Some(engine))
                            .map(|text| Some(text.into_bytes()))
                    })
                    .collect::<Result<Vec<_>, SQLError>>()?;
                transport.send(&BackendMessage::DataRow(values))?;
            }
        }
        SQLResultKind::Command => {}
        SQLResultKind::Unknown => {
            return Err(
                SQLError::Internal("SQL result descriptor presence is unknown".into()).into(),
            );
        }
    }
    transport.send(&match &result.command_tag {
        Some(tag) => BackendMessage::CommandComplete(tag.clone()),
        None => BackendMessage::EmptyQueryResponse,
    })
}

pub(crate) fn send_notices(transport: &mut Transport, engine: &Engine) -> Result<(), ServerError> {
    for (level, message) in engine.take_sql_notices() {
        let mut notice = ErrorOrNotice::error("00000", message);
        notice.severity = match level.as_str() {
            "WARNING" => {
                notice.code = "01000".into();
                NoticeSeverity::Warning
            }
            "INFO" => NoticeSeverity::Info,
            "LOG" => NoticeSeverity::Log,
            "DEBUG" => NoticeSeverity::Debug,
            _ => NoticeSeverity::Notice,
        };
        transport.send(&BackendMessage::NoticeResponse(notice))?;
    }
    Ok(())
}

pub(crate) fn sql_error(error: &SQLError) -> ErrorOrNotice {
    ErrorOrNotice::error(error.sqlstate().unwrap_or("XX000"), error.to_string())
}
