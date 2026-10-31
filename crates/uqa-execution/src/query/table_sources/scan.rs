//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//
//! Demand-driven local and materialized table scans.

use super::TableScanContext;
use crate::query::{
    local_table::{
        table_lock_origin_with_control, HierarchyRowSource, LocalTableRowSource,
        LocalTableScanConfig,
    },
    CteScope,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionMap, ProductionVec},
    Value,
};
mod schema;
use schema::{append_scan_metadata, projected_scan_schema_with_control};
use uqa_sql::{
    plan::{
        source_projection::{ColumnPrune, QualifierFilters, SourceProjection},
        SourcePlan,
    },
    semantics::{
        source_filters::{has_filters_for_qualifier, qualifier_filter_with_control},
        TABLE_OID_COLUMN, XMIN_COLUMN,
    },
    SQLError, SQLParam,
};
type StreamingLocalTableScan<'a> = (Box<dyn crate::PhysicalOperator + 'a>, bool);

pub fn try_streaming_local_table_scan<'a, S: Clone>(
    context: TableScanContext<'_>,
    source: &SourcePlan,
    ctes: &CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    params: &[SQLParam],
) -> Result<Option<StreamingLocalTableScan<'a>>, SQLError> {
    try_streaming_local_table_scan_with_control(
        context,
        source,
        ctes,
        prune,
        filters,
        params,
        &ProductionControl::uncontrolled(),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves source schema and row identity"
)]
pub fn try_streaming_local_table_scan_with_control<'a, S: Clone>(
    context: TableScanContext<'_>,
    source: &SourcePlan,
    ctes: &CteScope<S>,
    prune: Option<&ColumnPrune>,
    filters: Option<&QualifierFilters>,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<Option<StreamingLocalTableScan<'a>>, SQLError> {
    control.check()?;
    let SourcePlan::Table {
        executable_query: _,
        name,
        qualifier,
        alias,
        column_aliases,
        bound_columns,
        include_descendants,
    } = source
    else {
        return Ok(None);
    };
    let qualifier = alias.as_deref().unwrap_or(qualifier);
    if let Some(materialized) = ctes.materialized_for_scan(name) {
        if has_filters_for_qualifier(filters, qualifier) {
            return Ok(None);
        }
        let scan = materialized.into_operator_with_control(control)?;
        let selection = crate::ColumnSelection::with_scan_projection_with_control(
            scan,
            qualifier,
            column_aliases,
            prune.and_then(|prune| prune.get(qualifier)),
            ctes.lock_identities.emit,
            control,
        )?;
        return Ok(Some((selection, false)));
    }
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution_with_control(control)?;
    if catalog
        .view_resolved_with_control(&resolution, name, control)?
        .is_some()
        || catalog
            .foreign_table_resolved_with_control(&resolution, name, control)?
            .is_some()
    {
        return Ok(None);
    }
    let Some(root_name) = catalog.table_name_resolved_with_control(&resolution, name, control)?
    else {
        return Ok(None);
    };
    let root_table = catalog
        .table_resolved_with_control(&resolution, &root_name, control)?
        .ok_or_else(|| SQLError::UnknownTable(name.clone()))?;
    let wanted = prune.and_then(|prune| prune.get(qualifier));
    let metadata = wanted.map(SourceProjection::metadata).unwrap_or_default();
    let table_columns = uqa_sql::semantics::bound_source_column_names_with_control(
        root_table.columns.iter().map(|column| column.name.as_str()),
        bound_columns.as_deref(),
        control,
    )?;
    if column_aliases.len() > table_columns.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: format!(
                "table \"{qualifier}\" has {} columns available but {} columns specified",
                table_columns.len(),
                column_aliases.len()
            ),
        });
    }
    let open_document = root_table.columns.is_empty() && !root_table.columns_declared;
    let mut selected = ProductionVec::new(*control);
    for column in table_columns.iter() {
        selected.push_copy(column.as_str())?;
    }
    if open_document {
        if let Some(wanted) = wanted {
            for field in requested_document_fields(wanted) {
                selected.push_copy(field)?;
            }
        }
    }
    // Aliases retain their declaration positions even when a projection omits preceding fields.
    let mut visible_names = ProductionMap::new(*control);
    let mut columns = ProductionVec::new(*control);
    let mut schema = ProductionVec::new(*control);
    for (position, physical) in selected.iter().enumerate() {
        let visible = column_aliases
            .get(position)
            .map_or(*physical, String::as_str);
        visible_names.insert_copy(*physical, visible)?;
        if wanted.is_none_or(|wanted| wanted.contains(visible)) {
            columns.push_produced(control.copy_text(physical)?)?;
            schema.push_produced(control.copy_text(visible)?)?;
        }
    }
    let include_table_oid = wanted.as_ref().is_some_and(|wanted| {
        wanted
            .requested_columns()
            .any(|column| column == TABLE_OID_COLUMN)
    });
    let include_xmin = wanted.as_ref().is_some_and(|wanted| {
        wanted
            .requested_columns()
            .any(|column| column == XMIN_COLUMN)
    });
    if include_xmin {
        columns.push_produced(control.copy_text(XMIN_COLUMN)?)?;
        schema.push_produced(control.copy_text(XMIN_COLUMN)?)?;
    }
    if include_table_oid {
        schema.push_produced(control.copy_text(TABLE_OID_COLUMN)?)?;
    }
    let columns = columns.finish()?;
    let schema = schema.finish()?;
    let identity = uqa_core::RelationIdentity::try_borrowed_reference(&root_name)
        .map_err(|error| SQLError::Internal(error.to_string()))?
        .identity_with_control(uqa_core::RelationName::decoded("public"), control)?;
    let row_type = root_table
        .catalog_oids
        .row_type_reference_with_control(&identity, control)?;
    let mut physical_schema = projected_scan_schema_with_control(
        qualifier,
        &columns,
        &schema,
        &root_table.columns,
        include_table_oid,
        row_type.as_deref(),
        control,
    )?;
    if open_document {
        physical_schema = crate::RowSchema::with_wildcard_hidden_positions_with_control(
            &physical_schema,
            0..schema.len(),
            control,
        )?;
    }
    physical_schema = append_scan_metadata(&physical_schema, qualifier, metadata, control)?;
    let table_names = catalog.hierarchy_scan_tables_with_control(
        &resolution,
        name,
        *include_descendants,
        control,
    )?;
    let mut sources = ProductionVec::new(*control);
    let mut filter_pushed = false;
    for table_name in table_names.iter() {
        control.check()?;
        let table = context.tables.table(table_name)?;
        let table_snapshot = catalog
            .table_resolved_with_control(&resolution, table_name, control)?
            .ok_or_else(|| SQLError::UnknownTable(table_name.clone()))?;
        let column_definitions = table_snapshot.columns.clone();
        let lock_origin = table_lock_origin_with_control(
            &catalog,
            &resolution,
            table_name,
            qualifier,
            ctes.lock_identities.emit,
            control,
        )?;
        // Candidate selection and row evaluation must share the selected SQL operand types. A lossy column cast cannot use an index over the uncast values.
        let predicate_expression = qualifier_filter_with_control(filters, qualifier, control)?
            .map(|predicate| {
                uqa_sql::bind_type_introspection_owned(
                    predicate,
                    &physical_schema,
                    params,
                    None,
                    control,
                )
            })
            .transpose()?;
        let predicate = predicate_expression
            .as_ref()
            .filter(|predicate| !expression_references_tableoid(predicate))
            .map(|predicate| {
                crate::ProjectedPredicate::compile_bound_with_control(
                    predicate,
                    &physical_schema,
                    params,
                    control,
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
        // A filter that names its rows by identity, or by an integer primary key whose values are the identities, reads only those rows, from storage or from the changes above it, instead of scanning the table.
        let mut candidates = if recheck_pins.is_none() {
            predicate_expression.as_ref().and_then(|filter| {
                crate::query::key_candidates::key_candidates(
                    filter,
                    params,
                    crate::query::key_candidates::IdentityColumns::new(
                        &column_definitions,
                        table.maps_integer_keys(),
                        |name| visible_names.get(name).copied().unwrap_or(name),
                    ),
                )
            })
        } else {
            None
        }
        .map(|ids| {
            crate::query::SelectedIds::from_sorted(
                ids,
                context.runtime.work_mem_bytes()?,
                &context.runtime.cancellation_token(),
            )
        })
        .transpose()?;
        if candidates.is_none() && recheck_pins.is_none() {
            if let Some(filter) = &predicate_expression {
                candidates = crate::query::index_candidates::IndexCandidates {
                    reads: context.tables,
                    table: table_name,
                    columns: &column_definitions,
                    visible: &|name| visible_names.get(name).copied().unwrap_or(name).to_owned(),
                    params,
                    command_visible: ctes.reads_command_overlay(),
                    cancellation: &context.runtime.cancellation_token(),
                    bytes: context.runtime.work_mem_bytes()?,
                }
                .select(filter)?;
            }
        }
        let command_changes = if ctes.reads_command_overlay() {
            context.tables.command_overlay_changes(table_name)?
        } else {
            None
        };
        let estimated_cardinality = match &candidates {
            Some(candidates) => u64::try_from(candidates.len()).unwrap_or(u64::MAX),
            None => context.tables.table_row_estimate(table_name)?,
        };
        let table_oid =
            include_table_oid.then(|| Value::Int(i64::from(table_snapshot.catalog_oids.relation)));
        let name = control.copy_text(table_name)?;
        let copied_columns = copy_names(&columns, control)?;
        let copied_schema = copy_names(&schema, control)?;
        let (table_name, name_memory) = name.into_parts();
        let (columns, column_memory) = copied_columns.into_parts();
        let (schema, schema_memory) = copied_schema.into_parts();
        let config = control.finish(
            LocalTableScanConfig {
                cancellation: context.runtime.cancellation_token(),
                serializable: context.tables.serializable_read(&table_name)?,
                table_name,
                table,
                column_definitions,
                columns,
                schema,
                physical_schema: physical_schema.clone(),
                metadata,
                table_oid,
                predicate,
                estimated_cardinality,
                lock_origin,
                recheck_pins,
                candidates,
                command_changes,
            },
            control.combine(control.combine(name_memory, column_memory), schema_memory),
        )?;
        let source = LocalTableRowSource::from_produced(config)
            .with_scan_batch_bytes(context.runtime.work_mem_bytes()?);
        sources.push_produced(control.finish(source, control.empty_reservation())?)?;
    }
    let sources = sources.finish()?;
    let source: Box<dyn crate::RowSource> = if sources.len() == 1 {
        let (mut sources, _memory) = sources.into_parts();
        sources
            .pop()
            .expect("single-table scan owns its source")
            .into_boxed(control)?
    } else {
        HierarchyRowSource::boxed_with_control(sources, control)?
    };
    Ok(Some((
        crate::TableScan::boxed_with_control(source, control)?,
        filter_pushed,
    )))
}

fn expression_references_tableoid(expression: &crate::ScalarExpr) -> bool {
    let mut references = false;
    let projectable = expression
        .try_visit_columns(&mut |column| {
            references |= column == TABLE_OID_COLUMN;
            Ok::<_, std::convert::Infallible>(())
        })
        .unwrap_or_else(|never| match never {});
    projectable && references
}

fn copy_names(
    names: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for name in names {
        output.push_produced(control.copy_text(name)?)?;
    }
    Ok(output.finish()?)
}

fn requested_document_fields(projection: &SourceProjection) -> impl Iterator<Item = &str> {
    let metadata = projection.metadata();
    projection
        .requested_columns()
        .filter(move |column| match *column {
            XMIN_COLUMN | TABLE_OID_COLUMN => false,
            uqa_sql::semantics::DOC_ID_COLUMN => !metadata.includes_doc_id(),
            uqa_sql::semantics::SCORE_COLUMN => !metadata.includes_score(),
            _ => true,
        })
}

#[cfg(test)]
fn projected_scan_schema(
    qualifier: &str,
    columns: &[String],
    visible: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
    include_table_oid: bool,
    row_type: Option<uqa_sql::ast::CompositeTypeReference>,
) -> Result<crate::RowSchema, SQLError> {
    projected_scan_schema_with_control(
        qualifier,
        columns,
        visible,
        definitions,
        include_table_oid,
        row_type.as_ref(),
        &ProductionControl::uncontrolled(),
    )
}

#[cfg(test)]
mod tests;
