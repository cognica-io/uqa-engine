//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apply the durable input shape of stored table references to current table scans.

use crate::{ColumnSelection, PhysicalOperator};
use uqa_sql::SQLError;

pub use uqa_sql::semantics::bound_source_column_names;

pub fn bound_source_operator<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    bound: Option<&[String]>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(bound) = bound else {
        return Ok(operator);
    };
    if operator.schema() == bound {
        return Ok(operator);
    }
    let mapping = bound
        .iter()
        .map(|name| {
            let position = operator
                .schema()
                .iter()
                .position(|column| column == name)
                .ok_or_else(|| SQLError::UnknownColumn(name.clone()))?;
            Ok((
                name.clone(),
                operator.row_schema().identities()[position].clone(),
                position,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    Ok(Box::new(ColumnSelection::with_identities(
        operator, mapping,
    )))
}

use uqa_sql::plan::source_projection::ColumnPrune;

pub fn qualify_source_operator_with_columns<'a>(
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    source_columns: &[String],
    qualifier: &str,
    prune: Option<&ColumnPrune>,
    aliases: &[String],
    rebind_lock_origins: bool,
) -> Box<dyn crate::PhysicalOperator + 'a> {
    let mapping = source_columns
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            let source_base = operator.row_schema().public_name(index).unwrap_or(source);
            let column = aliases.get(index).map_or(source_base, String::as_str);
            if !qualifier.is_empty()
                && prune
                    .and_then(|prune| prune.get(qualifier))
                    .is_some_and(|wanted| !wanted.contains(column))
            {
                return None;
            }
            let identity = if qualifier.is_empty() {
                crate::ColumnIdentity::unqualified(column)
            } else {
                crate::ColumnIdentity::qualified(qualifier, column)
            };
            Some((column.to_string(), identity, index))
        })
        .collect();
    let selection = crate::ColumnSelection::with_identities(operator, mapping)
        .rebinding_score_sources(qualifier);
    if rebind_lock_origins {
        Box::new(selection.rebinding_lock_origins(qualifier))
    } else {
        Box::new(selection.discarding_lock_origins())
    }
}

pub use uqa_sql::semantics::join_alias_columns;

pub fn alias_join_operator<'a>(
    operator: Box<dyn crate::PhysicalOperator + 'a>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> Result<Box<dyn crate::PhysicalOperator + 'a>, SQLError> {
    let Some(alias) = alias else {
        if column_aliases.is_empty() {
            return Ok(operator);
        }
        return Err(SQLError::Internal(
            "JOIN column aliases exist without a relation alias".into(),
        ));
    };
    let columns = join_alias_columns(operator.row_schema(), alias, column_aliases)?;
    let mapping = columns
        .into_iter()
        .enumerate()
        .map(|(position, column)| {
            (
                column.clone(),
                crate::ColumnIdentity::qualified(alias, column),
                position,
            )
        })
        .collect();
    Ok(Box::new(
        crate::ColumnSelection::with_fresh_identities(operator, mapping)
            .rebinding_score_sources(alias),
    ))
}
