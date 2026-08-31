//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Demand-driven local and materialized table scans.

use super::{
    has_filters_for_qualifier, qualifier_filter, qualifier_for, table_lock_origin, Arc,
    ColumnPrune, CteScope, Engine, EngineHierarchyRowSource, EngineTableRowSource,
    QualifierFilters, SQLError, SQLParam, SourcePlan, StreamingLocalTableScan, Value,
    TABLE_OID_COLUMN, XMIN_COLUMN,
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
        include_descendants,
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
                if prune
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
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    if catalog.view_resolved(&resolution, name)?.is_some()
        || catalog.foreign_table_resolved(&resolution, name)?.is_some()
    {
        return Ok(None);
    }
    let Some(root_name) = catalog.table_name_resolved(&resolution, name)? else {
        return Ok(None);
    };
    let root_table = catalog
        .table_resolved(&resolution, &root_name)?
        .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
    let wanted = prune.and_then(|prune| prune.get(&qualifier)).cloned();
    let metadata = wanted
        .as_ref()
        .map(super::super::SourceProjection::metadata)
        .unwrap_or_default();
    let table_columns = root_table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    // An unqualified reference is conservatively requested from every FROM source during pruning. The scan schema must still describe only real table columns: advertising those over-inclusive requests as columns can make later joins bind an unqualified name to a non-existent value.
    let mut columns = match wanted.as_ref() {
        Some(wanted) => table_columns
            .into_iter()
            .filter(|column| wanted.contains(column))
            .collect(),
        None => table_columns,
    };
    let include_table_oid = wanted
        .as_ref()
        .is_some_and(|wanted| wanted.contains(TABLE_OID_COLUMN));
    let include_xmin = wanted
        .as_ref()
        .is_some_and(|wanted| wanted.contains(XMIN_COLUMN));
    if include_xmin {
        columns.push(XMIN_COLUMN.into());
    }
    let mut schema = columns.clone();
    if include_table_oid {
        schema.push(TABLE_OID_COLUMN.into());
    }
    let root_column_definitions = &root_table.columns;
    let mut column_types = columns
        .iter()
        .map(|column| {
            root_column_definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
                .or_else(|| (column == XMIN_COLUMN).then_some(uqa_sql::ast::ColumnType::Xid))
        })
        .collect::<Vec<_>>();
    if include_table_oid {
        column_types.push(Some(uqa_sql::ast::ColumnType::Oid));
    }
    let mut physical_schema =
        uqa_execution::RowSchema::with_qualified_types(&qualifier, schema.clone(), column_types);
    if let Some(position) = schema.iter().position(|column| column == XMIN_COLUMN) {
        physical_schema = uqa_execution::RowSchema::with_wildcard_hidden_positions(
            &physical_schema,
            vec![position],
        );
    }
    let metadata_relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut metadata_attributes = Vec::with_capacity(2);
    if metadata.includes_doc_id() {
        metadata_attributes.push((
            metadata_relation.column(0),
            Some(uqa_sql::ast::ColumnType::BigInteger),
        ));
    }
    if metadata.includes_score() {
        metadata_attributes.push((
            metadata_relation.column(1),
            Some(uqa_sql::ast::ColumnType::DoublePrecision),
        ));
    }
    if !metadata_attributes.is_empty() {
        physical_schema =
            uqa_execution::RowSchema::append_internal_typed(&physical_schema, &metadata_attributes);
        let mut aliases = Vec::with_capacity(metadata_attributes.len() * 3);
        if metadata.includes_doc_id() {
            let column = metadata_relation.column(0);
            let slot = physical_schema
                .internal_slot(column)
                .expect("document metadata attribute must have a physical slot");
            let ty = Some(uqa_sql::ast::ColumnType::BigInteger);
            aliases.push((
                uqa_execution::ColumnIdentity::qualified(
                    crate::sql::META_QUALIFIER,
                    crate::sql::META_DOC_ID_COLUMN,
                ),
                slot,
                ty.clone(),
            ));
            if !physical_schema.has_qualified_column(&qualifier, crate::sql::DOC_ID_COLUMN) {
                aliases.push((
                    uqa_execution::ColumnIdentity::qualified(&qualifier, crate::sql::DOC_ID_COLUMN),
                    slot,
                    ty.clone(),
                ));
            }
            if !physical_schema.has_unqualified_column(crate::sql::DOC_ID_COLUMN) {
                aliases.push((
                    uqa_execution::ColumnIdentity::unqualified(crate::sql::DOC_ID_COLUMN),
                    slot,
                    ty,
                ));
            }
        }
        if metadata.includes_score() {
            let column = metadata_relation.column(1);
            let slot = physical_schema
                .internal_slot(column)
                .expect("score metadata attribute must have a physical slot");
            let ty = Some(uqa_sql::ast::ColumnType::DoublePrecision);
            aliases.push((
                uqa_execution::ColumnIdentity::qualified(
                    crate::sql::META_QUALIFIER,
                    crate::sql::META_SCORE_COLUMN,
                ),
                slot,
                ty.clone(),
            ));
            if !physical_schema.has_qualified_column(&qualifier, crate::sql::SCORE_COLUMN) {
                aliases.push((
                    uqa_execution::ColumnIdentity::qualified(&qualifier, crate::sql::SCORE_COLUMN),
                    slot,
                    ty.clone(),
                ));
            }
            if !physical_schema.has_unqualified_column(crate::sql::SCORE_COLUMN) {
                aliases.push((
                    uqa_execution::ColumnIdentity::unqualified(crate::sql::SCORE_COLUMN),
                    slot,
                    ty,
                ));
            }
        }
        physical_schema =
            uqa_execution::RowSchema::with_physical_identity_aliases(&physical_schema, &aliases);
    }
    let table_names = catalog.hierarchy_scan_tables(&resolution, name, *include_descendants)?;
    let mut sources = Vec::with_capacity(table_names.len());
    let mut filter_pushed = false;
    for table_name in table_names {
        let table = engine.require_query_table(&table_name)?;
        let column_definitions = catalog
            .table_resolved(&resolution, &table_name)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?
            .columns
            .clone();
        let lock_origin = table_lock_origin(
            &catalog,
            &resolution,
            &table_name,
            &qualifier,
            ctes.lock_identities.emit,
        )?;
        let predicate_expression = qualifier_filter(filters, &qualifier);
        let predicate = predicate_expression
            .filter(|predicate| !expression_references_tableoid(predicate))
            .map(|predicate| {
                uqa_execution::ProjectedPredicate::compile_with_schema(
                    &predicate,
                    &physical_schema,
                    params,
                )
            })
            .transpose()?
            .flatten();
        filter_pushed |= predicate.is_some();
        let recheck_pins = lock_origin
            .as_ref()
            .and_then(|(origin_qualifier, storage_name)| {
                ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
            });
        let command_changes = if ctes.reads_command_overlay() {
            engine.command_overlay_changes(&table_name)?.map(Arc::new)
        } else {
            None
        };
        let estimated_cardinality = engine.table_doc_count(&table_name)?;
        let table_oid = include_table_oid
            .then(|| {
                crate::sql::catalog::snapshot_table_relation_oid(&catalog, &resolution, &table_name)
            })
            .transpose()?
            .map(Value::Int);
        sources.push(EngineTableRowSource {
            table_name,
            table,
            column_definitions,
            columns: columns.clone(),
            schema: schema.clone(),
            physical_schema: physical_schema.clone(),
            metadata,
            table_oid,
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
        });
    }
    let source: Box<dyn uqa_execution::RowSource> = if sources.len() == 1 {
        Box::new(
            sources
                .pop()
                .ok_or_else(|| SQLError::Internal("single-table scan lost its source".into()))?,
        )
    } else {
        Box::new(EngineHierarchyRowSource::new(sources)?)
    };
    Ok(Some((
        Box::new(uqa_execution::TableScan::new(source)),
        filter_pushed,
    )))
}

fn expression_references_tableoid(expression: &uqa_execution::ScalarExpr) -> bool {
    let mut columns = std::collections::BTreeSet::new();
    expression.collect_columns(&mut columns) && columns.contains(TABLE_OID_COLUMN)
}
