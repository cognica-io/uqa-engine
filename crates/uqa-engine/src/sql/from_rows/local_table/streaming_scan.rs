//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Demand-driven local and materialized table scans.

use super::{
    has_filters_for_qualifier, is_score_provenance_column, qualifier_filter, qualifier_for,
    table_lock_origin, Arc, ColumnPrune, CteScope, Engine, EngineTableRowSource, QualifierFilters,
    SQLError, SQLParam, SourcePlan, StreamingLocalTableScan,
};

pub(in crate::sql) fn try_streaming_local_table_scan<'a>(
    engine: &Engine,
    source: &SourcePlan,
    ctes: &CteScope,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    params: &[SQLParam],
) -> Result<Option<StreamingLocalTableScan<'a>>, SQLError> {
    let SourcePlan::Table {
        name,
        qualifier,
        alias,
    } = source
    else {
        return Ok(None);
    };
    let qualifier = qualifier_for(qualifier, alias.as_deref());
    if let Some(materialized) = ctes.rows.get(name).cloned() {
        if has_filters_for_qualifier(filters, &qualifier) {
            return Ok(None);
        }
        let mapping = materialized
            .row_schema()
            .identities()
            .iter()
            .enumerate()
            .filter_map(|(position, identity)| {
                let column = identity.column();
                if !is_score_provenance_column(column)
                    && prune
                        .and_then(|prune| prune.get(&qualifier))
                        .is_some_and(|wanted| !wanted.contains(column))
                {
                    return None;
                }
                let output_identity = uqa_execution::ColumnIdentity::qualified(&qualifier, column);
                Some((column.to_string(), output_identity, position))
            })
            .collect();
        let scan: Box<dyn uqa_execution::PhysicalOperator + 'a> =
            Box::new(uqa_execution::SharedSpillScan::new(materialized));
        let selection = uqa_execution::ColumnSelection::with_identities(scan, mapping);
        let selection = if ctes.lock_identities.emit {
            selection.rebinding_lock_origins(qualifier)
        } else {
            selection
        };
        return Ok(Some((Box::new(selection), false)));
    }
    if engine.view_plan(name)?.is_some()
        || engine
            .foreign_table(name)
            .map_err(SQLError::Unsupported)?
            .is_some()
    {
        return Ok(None);
    }
    let Some(table) = engine
        .try_table(name)
        .map_err(|error| SQLError::Internal(format!("resolve table `{name}`: {error}")))?
    else {
        return Ok(None);
    };
    let wanted = prune.and_then(|prune| prune.get(&qualifier)).cloned();
    let table_columns = engine
        .try_table_columns(name)
        .map_err(|error| SQLError::Internal(format!("read table columns for `{name}`: {error}")))?;
    // An unqualified reference is conservatively requested from every FROM source during pruning. The scan schema must still describe only real table columns: advertising those over-inclusive requests as columns can make later joins bind an unqualified name to a non-existent value.
    let columns = match wanted.as_ref() {
        Some(wanted) => table_columns
            .into_iter()
            .filter(|column| wanted.contains(column))
            .collect(),
        None => table_columns,
    };
    let schema = columns.clone();
    let column_definitions = table.columns.read().clone();
    let column_types = columns
        .iter()
        .map(|column| {
            column_definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    let lock_origin = table_lock_origin(engine, name, &qualifier, ctes.lock_identities.emit)?;
    let physical_schema =
        uqa_execution::RowSchema::with_qualified_types(&qualifier, schema.clone(), column_types);
    let predicate = qualifier_filter(filters, &qualifier)
        .map(|predicate| {
            uqa_execution::ProjectedPredicate::compile_with_schema(
                &predicate,
                &physical_schema,
                params,
            )
        })
        .transpose()?
        .flatten();
    let filter_pushed = predicate.is_some();
    let recheck_pins = lock_origin
        .as_ref()
        .and_then(|(origin_qualifier, storage_name)| {
            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
        });
    let command_changes = if ctes.reads_command_overlay() {
        engine.command_overlay_changes(name)?.map(Arc::new)
    } else {
        None
    };
    let estimated_cardinality = engine.table_doc_count(name)?;
    let source = EngineTableRowSource {
        table_name: name.clone(),
        table,
        column_definitions,
        columns,
        schema,
        physical_schema,
        predicate,
        estimated_cardinality,
        after: None,
        lock_origin,
        recheck_pins,
        recheck_cursor: 0,
        command_changes,
        command_change_after: None,
        command_base_after: None,
        command_base_ids: std::collections::VecDeque::new(),
        command_base_exhausted: false,
    };
    Ok(Some((
        Box::new(uqa_execution::TableScan::new(Box::new(source))),
        filter_pushed,
    )))
}
