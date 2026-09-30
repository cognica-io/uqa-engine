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
    for notice in engine.take_sql_notices() {
        let mut response = ErrorOrNotice::error(notice.sqlstate, notice.message);
        response.severity = match notice.severity {
            uqa_engine::NoticeSeverity::Warning => NoticeSeverity::Warning,
            uqa_engine::NoticeSeverity::Notice => NoticeSeverity::Notice,
            uqa_engine::NoticeSeverity::Info => NoticeSeverity::Info,
            uqa_engine::NoticeSeverity::Log => NoticeSeverity::Log,
            uqa_engine::NoticeSeverity::Debug => NoticeSeverity::Debug,
        };
        response.detail = notice.detail;
        response.hint = notice.hint;
        transport.send(&BackendMessage::NoticeResponse(response))?;
    }
    Ok(())
}

pub(crate) fn sql_error(error: &SQLError) -> ErrorOrNotice {
    let mut response = ErrorOrNotice::error(error.sqlstate().unwrap_or("XX000"), error.to_string());
    if let SQLError::Diagnostic { detail, hint, .. } = error {
        response.detail.clone_from(detail);
        response.hint.clone_from(hint);
    }
    response
}
