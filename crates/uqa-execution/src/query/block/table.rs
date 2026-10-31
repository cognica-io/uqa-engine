//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-table access-path execution.

use uqa_sql::schema::ScalarTypeSchema;

use super::{
    build_facet_output, combine_filter_parts, execute_mixed_where,
    execute_query_block_operator_output, expand_from_star_columns,
    expr_contains_jsonpath_fts_match, expr_is_jsonpath_fts_match, facet_projection_fields,
    flatten_and_filter_parts, post_retrieval_score_top_k, projection_columns,
    score_limited_text_filter, score_order_top_k, AccessPathPlan, BoundSingleRelation, CteScope,
    FacetExecution, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam, ScalarExpr,
    ScoredDocumentSource, ScoredInput, SingleRelation, SourceContext, SourceProjection,
    TABLE_OID_COLUMN,
};

#[expect(
    clippy::too_many_lines,
    reason = "preserves SELECT schema and row identity"
)]
pub fn run_single_table_select_output<'a, S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'a, S>,
    bound_relation: BoundSingleRelation<'_>,
    block: &'a QueryBlockPlan,
    stmt: &'a QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &'a CteScope<S>,
    output_mode: QueryOutputMode<'a>,
) -> Result<QueryOutput, SQLError> {
    let BoundSingleRelation {
        relation:
            SingleRelation {
                reference_name,
                relation_name: table,
                qualifier,
            },
        schema,
    } = bound_relation;
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    let table_snapshot = catalog
        .table_resolved(&resolution, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let open_document = table_snapshot.columns.is_empty() && !table_snapshot.columns_declared;
    let has_stored_score_column = table_snapshot
        .columns
        .iter()
        .any(|column| column.name == super::SCORE_COLUMN);
    let predicate = stmt
        .r#where
        .as_ref()
        .map(|predicate| {
            Ok::<_, SQLError>(
                context
                    .relational
                    .evaluator(params, ctes)?
                    .bind_type_introspection(predicate.clone(), schema),
            )
        })
        .transpose()?;
    let score_top_k = if !has_stored_score_column
        && matches!(
            block.access,
            AccessPathPlan::OperatorTree {
                score_limit_pushdown: true
            }
        ) {
        score_order_top_k(stmt, context, params, ctes)?
            .filter(|_| score_limited_text_filter(stmt.r#where.as_ref()))
    } else {
        None
    };
    let post_retrieval_top_k = if has_stored_score_column {
        None
    } else {
        post_retrieval_score_top_k(stmt, context, params, ctes)?
    };
    let has_jsonpath_fts_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(expr_contains_jsonpath_fts_match);
    // Try the operator-tree pipeline first: lower the WHERE clause to
    // an `OperatorTree`, run `QueryOptimizer` (10 algebraic / graph-
    // aware / fusion-reordering passes - compatibility), then execute
    // through `PlanExecutor` against an `EngineDriver`. The bridge
    // returns `None` for shapes that are not posting-list access paths
    // (arithmetic across columns, subqueries, window calls, ...); those
    // remain scalar predicates in this relational filter node.
    let retrieval = context.relation_retrieval;
    let optimised: Option<crate::query::scored_input::ScoredEntriesProducer<'_>> =
        if has_jsonpath_fts_filter || !matches!(block.access, AccessPathPlan::OperatorTree { .. }) {
            None
        } else if let (Some(top_k), Some(ScalarExpr::Func { name, args, .. })) =
            (score_top_k, stmt.r#where.as_ref())
        {
            Some(Box::new(move || {
                retrieval
                    .function(table, reference_name, name, args, params, Some(top_k))
                    .map(Into::into)
            }))
        } else {
            retrieval.prepare_accelerated(
                table,
                reference_name,
                predicate.as_ref(),
                params,
                post_retrieval_top_k.is_none(),
            )?
        };
    let score_bearing_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(uqa_sql::semantics::contains_retrieval);
    let mut pending = optimised;
    let (mut scored, mut physical_filter) = if pending.is_some() {
        (ScoredInput::entries(Vec::new(), score_bearing_filter), None)
    } else {
        // Access classification precedes runtime constant binding. If lowering declines a search composition, its search leaves still require the mixed executor; an ordinary scalar filter cannot execute them.
        let access = match &block.access {
            AccessPathPlan::OperatorTree { .. } if score_bearing_filter => &AccessPathPlan::Hybrid,
            access => access,
        };
        match access {
            AccessPathPlan::Row => (ScoredInput::All, predicate.clone()),
            AccessPathPlan::Hybrid => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter) => {
                        pending = Some(Box::new(move || {
                            execute_mixed_where(
                                context,
                                table,
                                reference_name,
                                qualifier,
                                filter,
                                params,
                                ctes,
                            )
                            .map(Into::into)
                        }));
                        ScoredInput::entries(
                            Vec::new(),
                            uqa_sql::semantics::contains_retrieval(filter),
                        )
                    }
                    None => ScoredInput::All,
                };
                (rows, None)
            }
            AccessPathPlan::OperatorTree { .. } => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter_expr @ ScalarExpr::Func { name, args, .. })
                        if uqa_sql::registry::is_registered(name)
                            && !expr_is_jsonpath_fts_match(filter_expr) =>
                    {
                        pending = Some(Box::new(move || {
                            retrieval
                                .function(table, reference_name, name, args, params, None)
                                .map(Into::into)
                        }));
                        ScoredInput::entries(
                            Vec::new(),
                            uqa_sql::semantics::contains_retrieval(filter_expr),
                        )
                    }
                    // The planner may optimistically choose the operator-tree
                    // access class for a predicate that the posting-list IR
                    // cannot represent (for example `IS NULL`, arithmetic, or
                    // a subquery). Keep it inside the same physical query
                    // pipeline as a relational Filter over the table scan.
                    Some(_) | None => ScoredInput::All,
                };
                let filter = matches!(rows, ScoredInput::All)
                    .then(|| predicate.clone())
                    .flatten();
                (rows, filter)
            }
        }
    };

    let source_projection = if let Some(source) = stmt.from.as_ref() {
        context
            .planning
            .column_prune_with_filter(stmt, source, physical_filter.as_ref(), ctes, Some(schema))?
            .and_then(|prune| prune.get(qualifier).cloned())
    } else {
        None
    };
    let metadata_projection = source_projection
        .as_ref()
        .map(SourceProjection::metadata)
        .unwrap_or_default();
    let bound_columns = match stmt.from.as_ref() {
        Some(uqa_sql::plan::SourcePlan::Table { bound_columns, .. }) => bound_columns.as_deref(),
        _ => None,
    };
    let table_columns = uqa_sql::semantics::bound_source_column_names(
        table_snapshot
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        bound_columns,
    )?;
    let source_schema = selected_source_columns(
        table_columns,
        source_projection.as_ref(),
        schema,
        open_document,
    );

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        let execution = FacetExecution {
            fields: &facet_fields,
            source_schema,
            params,
            ctes,
            output_mode,
        };
        return build_facet_output(
            context,
            table,
            scored,
            pending,
            physical_filter.take(),
            execution,
        );
    }

    let table_state = context.scans.tables.table(table)?;
    // A filter that names its rows by `_doc_id` or by an integer primary key whose values are the rows' identities reads only those of them that exist and evaluates the whole filter on them.
    if matches!(scored, ScoredInput::All) {
        if let Some(identities) = physical_filter.as_ref().and_then(|filter| {
            crate::query::key_candidates::key_candidates(
                filter,
                params,
                crate::query::key_candidates::IdentityColumns::new(
                    &table_snapshot.columns,
                    table_state.maps_integer_keys(),
                    |name| name,
                ),
            )
        }) {
            let serializable = context.scans.tables.serializable_read(table)?;
            let changes = if ctes.reads_command_overlay() {
                context.scans.tables.command_overlay_changes(table)?
            } else {
                None
            };
            let documents = table_state.read_documents();
            let mut entries = Vec::with_capacity(identities.len());
            for doc_id in identities {
                let changed = changes
                    .as_ref()
                    .map(|changes| changes.change_presence(doc_id))
                    .transpose()
                    .map_err(|error| {
                        crate::storage_errors::storage_error(
                            "probe a changed document identity",
                            &error,
                        )
                    })?
                    .flatten();
                let present = match changed {
                    Some(present) => present,
                    None => documents.contains_doc_id(doc_id).map_err(|error| {
                        crate::storage_errors::storage_error(
                            "probe a named document identity",
                            &error,
                        )
                    })?,
                };
                if present {
                    entries.push(uqa_core::ScoredEntry { doc_id, score: 0.0 });
                } else if let Some(serializable) = serializable.as_ref() {
                    // The read of a named identity that holds no row still depends on that identity; the source observes the rows it returns.
                    serializable.observe_row(doc_id)?;
                }
            }
            drop(documents);
            scored = ScoredInput::entries(entries, false);
        }
    }
    if matches!(scored, ScoredInput::All) {
        if let Some(filter) = physical_filter.as_ref() {
            if let Some(identities) = (crate::query::index_candidates::IndexCandidates {
                reads: context.scans.tables,
                table,
                columns: &table_snapshot.columns,
                visible: &str::to_owned,
                params,
                command_visible: ctes.reads_command_overlay(),
                cancellation: &context.scans.runtime.cancellation_token(),
                bytes: context.scans.runtime.work_mem_bytes()?,
            })
            .select(filter)?
            {
                scored = ScoredInput::Candidates(identities);
            }
        }
    }
    let ordered_primary_key = match table_snapshot
        .columns
        .iter()
        .find(|column| column.primary_key && column.ty.is_integer())
    {
        Some(column) if identities_follow_keys(table_state.as_ref())? => Some(column.name.clone()),
        _ => None,
    };
    let column_metadata = |name: &String| {
        let definition = table_snapshot
            .columns
            .iter()
            .find(|column| column.name == *name);
        (
            name.clone(),
            definition.map(|column| column.ty.clone()),
            definition.and_then(|column| column.collation_oid),
        )
    };
    // A pushed predicate reads the stored fields alone. A column the scan attaches from row metadata is left to the residual filter, which sees the whole row.
    let stored_fields = source_schema
        .iter()
        .filter(|name| {
            !crate::query::scored_input::is_attached_metadata_column(name, &table_snapshot.columns)
        })
        .cloned()
        .collect::<Vec<_>>();
    let stored_schema = crate::RowSchema::with_column_metadata(
        Some(qualifier),
        stored_fields.iter().map(column_metadata),
    );
    let (pushed_predicate, residual_filter) =
        split_projected_filter(physical_filter.take(), &stored_schema, params)?;
    physical_filter = residual_filter;
    let cutoff =
        post_retrieval_top_k.filter(|_| pushed_predicate.is_none() && physical_filter.is_none());
    if let Some(top_k) = cutoff {
        scored.retain_top_scores_with_ties(top_k);
    }
    let lock_origin = if ctes.lock_identities.emit {
        let storage_name = catalog
            .table_name_resolved(&resolution, table)?
            .unwrap_or_else(|| table.to_string());
        Some((
            std::sync::Arc::<str>::from(qualifier),
            std::sync::Arc::<str>::from(storage_name),
        ))
    } else {
        None
    };
    let recheck_pins = lock_origin
        .as_ref()
        .and_then(|(origin_qualifier, storage_name)| {
            ctes.recheck_docs_for_scan(origin_qualifier, storage_name)
        });
    // An access path that yields row identities needs the documents only for the projected fields. When the table's indexes hold all of them, the rows are projected from the index entries. A row-locking read and a pinned recheck read the current documents.
    let index_only = pending.is_some()
        && lock_origin.is_none()
        && recheck_pins.is_none()
        && context
            .scans
            .tables
            .index_holds_fields(table, &table_state, &source_schema)?;
    let source = ScoredDocumentSource::new_with_metadata(
        table,
        table_state,
        scored,
        source_schema,
        ordered_primary_key,
        pushed_predicate,
        metadata_projection,
    )
    .with_scan_batch_bytes(context.scans.runtime.work_mem_bytes()?)
    .with_index_read_control(context.scans.tables.index_read_control()?)
    .with_index_only(index_only)
    .with_serializable_read(context.scans.tables.serializable_read(table)?)
    .with_table_oid(crate::catalog::projection::snapshot_table_relation_oid(
        &catalog,
        &resolution,
        table,
    )?)
    .with_qualifier(qualifier)
    .with_row_type(qualifier, schema.whole_row_type(qualifier).cloned())?
    .with_lock_origin(lock_origin);
    let source = if open_document {
        source.without_wildcard_columns()
    } else {
        source
    };
    let source = crate::query::scored_input::defer_entries(
        source,
        pending,
        cutoff.filter(|_| score_bearing_filter),
        recheck_pins,
    )?;
    let source = context
        .scans
        .runtime
        .diagnostics
        .observe(source, "Relation Scan")?;
    let (source, columns) = crate::physical::construction::OperatorBuild::run(source, |source| {
        expand_from_star_columns(
            projection_columns(&stmt.projections),
            &stmt.projections,
            source.row_schema(),
        )
    })?;
    execute_query_block_operator_output(
        context.relational,
        source,
        physical_filter.as_ref(),
        stmt,
        block,
        params,
        ctes,
        columns,
        output_mode,
    )
}

/// Compile every independently supported top-level conjunct into the storage
/// projection. A subquery or another unsupported residual must not force
/// otherwise positional predicates back through the row scalar evaluator.
fn split_projected_filter(
    predicate: Option<ScalarExpr>,
    source_schema: &crate::RowSchema,
    params: &[SQLParam],
) -> Result<(Option<crate::ProjectedPredicate>, Option<ScalarExpr>), SQLError> {
    let Some(predicate) = predicate else {
        return Ok((None, None));
    };
    if expression_references_tableoid(&predicate) {
        return Ok((None, Some(predicate)));
    }
    if reads_stored_fields_only(&predicate, source_schema) {
        if let Some(compiled) =
            crate::ProjectedPredicate::compile_with_schema(&predicate, source_schema, params)?
        {
            return Ok((Some(compiled), None));
        }
    }
    if !matches!(predicate, ScalarExpr::And(_)) {
        return Ok((None, Some(predicate)));
    }

    let mut projected = Vec::new();
    let mut residual = Vec::new();
    for conjunct in flatten_and_filter_parts(&predicate) {
        if !expression_references_tableoid(conjunct)
            && reads_stored_fields_only(conjunct, source_schema)
            && crate::ProjectedPredicate::compile_with_schema(conjunct, source_schema, params)?
                .is_some()
        {
            projected.push(conjunct.clone());
        } else {
            residual.push(conjunct.clone());
        }
    }
    let projected = match combine_filter_parts(projected) {
        Some(expression) => Some(
            crate::ProjectedPredicate::compile_with_schema(&expression, source_schema, params)?
                .ok_or_else(|| {
                    SQLError::Internal(
                        "individually compiled projected predicates could not be combined".into(),
                    )
                })?,
        ),
        None => None,
    };
    Ok((projected, combine_filter_parts(residual)))
}

fn expression_references_tableoid(expression: &ScalarExpr) -> bool {
    let mut columns = std::collections::BTreeSet::new();
    expression.collect_columns(&mut columns) && columns.contains(TABLE_OID_COLUMN)
}

/// Whether a scan in identity order reads the rows of `table` in integer key order: the table maps its keys to identities and holds no row at an identity no key names, all of which lie above the identities keys name.
fn identities_follow_keys(
    table: &dyn crate::query::table_read::TableRead,
) -> Result<bool, SQLError> {
    if !table.maps_integer_keys() {
        return Ok(false);
    }
    let unmapped = table
        .read_documents()
        .next_doc_ids(
            Some(uqa_sql::semantics::key_identity::KEY_IDENTITY_LIMIT - 1),
            1,
        )
        .map_err(|error| {
            crate::storage_errors::storage_error("probe identities no key names", &error)
        })?;
    Ok(unmapped.is_empty())
}

/// Whether every column `expression` reads is one of the stored fields a pushed predicate sees. A column the scan attaches from row metadata, or the `_meta` namespace, is left to the residual filter.
fn reads_stored_fields_only(expression: &ScalarExpr, stored: &crate::RowSchema) -> bool {
    let mut stored_only = true;
    expression.visit(&mut |node| match node {
        ScalarExpr::Column(column) => stored_only &= stored.has_unqualified_column(column),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            stored_only &= stored.has_qualified_column(qualifier, column);
        }
        _ => {}
    });
    stored_only
}

/// Keep explicit hidden-column demands even when a whole-row or correlated expression also requires every declared field. The physical row retains declaration order; unresolved output aliases never become input columns.
fn selected_source_columns(
    table_columns: Vec<String>,
    projection: Option<&SourceProjection>,
    schema: &crate::RowSchema,
    open_document: bool,
) -> Vec<String> {
    let Some(projection) = projection else {
        return table_columns;
    };
    let mut requested = projection
        .requested_columns()
        .filter(|column| open_document || schema.has_unqualified_column(column))
        .collect::<std::collections::BTreeSet<_>>();
    let mut selected = Vec::new();
    for column in table_columns {
        if projection.contains(&column) {
            requested.remove(column.as_str());
            selected.push(column);
        }
    }
    selected.extend(requested.into_iter().map(str::to_owned));
    selected
}

#[cfg(test)]
mod tests;
