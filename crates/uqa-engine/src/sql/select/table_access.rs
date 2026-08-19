//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Single-table access-path execution.

use super::{
    build_facet_output, column_prune_for_stmt_with_filter, combine_filter_parts, execute_function,
    execute_function_with_top_k, execute_mixed_where, execute_query_block_operator_output,
    expand_from_star_columns, expr_contains_jsonpath_fts_match, expr_is_jsonpath_fts_match,
    facet_projection_fields, flatten_and_filter_parts, post_retrieval_score_top_k,
    projection_columns, score_limited_text_filter, score_order_top_k, AccessPathPlan, CteScope,
    Engine, FacetExecution, QueryBlockPlan, QueryOutput, QueryOutputMode, SQLError, SQLParam,
    ScalarExpr, ScoredDocumentSource, ScoredInput, SingleRelation,
};

pub(in crate::sql) fn run_single_table_select_output(
    engine: &Engine,
    relation: SingleRelation<'_>,
    block: &QueryBlockPlan,
    stmt: &QueryBlockPlan,
    params: &[SQLParam],
    ctes: &CteScope,
    output_mode: QueryOutputMode,
) -> Result<QueryOutput, SQLError> {
    let SingleRelation {
        storage_name: table,
        qualifier,
    } = relation;
    if let Some(filter) = stmt.r#where.as_ref() {
        crate::sql::validate_expr_text_match_fields(engine, table, filter)?;
    }
    let score_top_k = if matches!(
        block.access,
        AccessPathPlan::OperatorTree {
            score_limit_pushdown: true
        }
    ) {
        score_order_top_k(stmt, engine, params, ctes)?
            .filter(|_| score_limited_text_filter(stmt.r#where.as_ref()))
    } else {
        None
    };
    let post_retrieval_top_k = post_retrieval_score_top_k(stmt, engine, params, ctes)?;
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
    let optimised = if has_jsonpath_fts_filter
        || !matches!(block.access, AccessPathPlan::OperatorTree { .. })
    {
        None
    } else if let (Some(top_k), Some(ScalarExpr::Func { name, args, .. })) =
        (score_top_k, stmt.r#where.as_ref())
    {
        Some(execute_function_with_top_k(
            engine,
            table,
            name,
            args,
            params,
            Some(top_k),
        )?)
    } else {
        crate::operator_tree_bridge::run_accelerated(engine, table, stmt.r#where.as_ref(), params)?
    };
    let score_bearing_filter = stmt
        .r#where
        .as_ref()
        .is_some_and(uqa_planner::optimizer::contains_retrieval);
    let (mut scored, mut physical_filter) = if let Some(rows) = optimised {
        (ScoredInput::entries(rows, score_bearing_filter), None)
    } else {
        match &block.access {
            AccessPathPlan::Row => (ScoredInput::All, stmt.r#where.clone()),
            AccessPathPlan::Hybrid => {
                let rows = match stmt.r#where.as_ref() {
                    Some(filter) => ScoredInput::entries(
                        execute_mixed_where(engine, table, filter, params, ctes)?,
                        uqa_planner::optimizer::contains_retrieval(filter),
                    ),
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
                        ScoredInput::entries(
                            execute_function(engine, table, name, args, params)?,
                            uqa_planner::optimizer::contains_retrieval(filter_expr),
                        )
                    }
                    // The planner may optimistically choose the operator-tree
                    // access class for a predicate that the posting-list IR
                    // cannot represent (for example `IS NULL`, arithmetic, or
                    // a subquery). Keep it inside the same physical query
                    // pipeline as a relational Filter over the table scan.
                    Some(_) => ScoredInput::All,
                    None => ScoredInput::All,
                };
                let filter = matches!(rows, ScoredInput::All)
                    .then(|| stmt.r#where.clone())
                    .flatten();
                (rows, filter)
            }
        }
    };

    let source_schema = stmt
        .from
        .as_ref()
        .and_then(|source| {
            column_prune_for_stmt_with_filter(engine, stmt, source, physical_filter.as_ref())
        })
        .and_then(|prune| prune.get(table).cloned())
        .map(|columns| columns.into_iter().collect())
        .map_or_else(
            || {
                engine.try_table_columns(table).map_err(|error| {
                    SQLError::Internal(format!("read table columns for `{table}`: {error}"))
                })
            },
            Ok,
        )?;

    if let Some(facet_fields) = facet_projection_fields(&stmt.projections)? {
        let execution = FacetExecution {
            fields: &facet_fields,
            source_schema,
            params,
            ctes,
            output_mode,
        };
        return build_facet_output(engine, table, scored, physical_filter.take(), execution);
    }

    let table_state = engine.require_table(table)?;
    let ordered_primary_key = engine
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read table schema for `{table}`: {error}")))?
        .and_then(|columns| {
            columns
                .into_iter()
                .find(|column| column.primary_key && column.ty.is_integer())
                .map(|column| column.name)
        });
    let predicate_schema = uqa_execution::RowSchema::with_qualified_types(
        qualifier,
        source_schema.clone(),
        vec![None; source_schema.len()],
    );
    let (pushed_predicate, residual_filter) =
        split_projected_filter(physical_filter.take(), &predicate_schema, params)?;
    physical_filter = residual_filter;
    if pushed_predicate.is_none() && physical_filter.is_none() {
        if let Some(top_k) = post_retrieval_top_k {
            scored.retain_top_scores_with_ties(top_k);
        }
    }
    let lock_origin = if ctes.emit_lock_identities {
        let storage_name = engine
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
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
    let source = ScoredDocumentSource::new(
        table,
        table_state,
        scored,
        source_schema,
        ordered_primary_key,
        pushed_predicate,
    )
    .with_qualifier(qualifier)
    .with_lock_origin(lock_origin)
    .with_recheck_pins(recheck_pins);
    let source: Box<dyn uqa_execution::PhysicalOperator + '_> =
        Box::new(uqa_execution::TableScan::new(Box::new(source)));
    let columns = expand_from_star_columns(
        projection_columns(&stmt.projections),
        &stmt.projections,
        &predicate_schema,
    )?;
    execute_query_block_operator_output(
        engine,
        source,
        physical_filter,
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
    source_schema: &uqa_execution::RowSchema,
    params: &[SQLParam],
) -> Result<
    (
        Option<uqa_execution::ProjectedPredicate>,
        Option<ScalarExpr>,
    ),
    SQLError,
> {
    let Some(predicate) = predicate else {
        return Ok((None, None));
    };
    if let Some(compiled) =
        uqa_execution::ProjectedPredicate::compile_with_schema(&predicate, source_schema, params)?
    {
        return Ok((Some(compiled), None));
    }
    if !matches!(predicate, ScalarExpr::And(_)) {
        return Ok((None, Some(predicate)));
    }

    let mut projected = Vec::new();
    let mut residual = Vec::new();
    for conjunct in flatten_and_filter_parts(&predicate) {
        if uqa_execution::ProjectedPredicate::compile_with_schema(conjunct, source_schema, params)?
            .is_some()
        {
            projected.push(conjunct.clone());
        } else {
            residual.push(conjunct.clone());
        }
    }
    let projected = match combine_filter_parts(projected) {
        Some(expression) => Some(
            uqa_execution::ProjectedPredicate::compile_with_schema(
                &expression,
                source_schema,
                params,
            )?
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use tempfile::tempdir;
    use uqa_core::DocId;
    use uqa_storage::document_store::Document;
    use uqa_storage::{DocumentStore, MemoryDocumentStore, StorageBackendResult};

    use super::Engine;

    type ProjectionLog = Arc<Mutex<Vec<Vec<String>>>>;
    type DocumentBatchLog = Arc<Mutex<Vec<Vec<DocId>>>>;

    struct ProjectionRecordingStore {
        inner: Box<dyn DocumentStore>,
        projections: ProjectionLog,
        document_batches: DocumentBatchLog,
    }

    impl DocumentStore for ProjectionRecordingStore {
        fn put(&mut self, doc_id: DocId, document: Document) -> StorageBackendResult<()> {
            self.inner.put(doc_id, document)
        }

        fn get(&self, doc_id: DocId) -> StorageBackendResult<Option<Document>> {
            self.inner.get(doc_id)
        }

        fn delete(&mut self, doc_id: DocId) -> StorageBackendResult<()> {
            self.inner.delete(doc_id)
        }

        fn clear(&mut self) -> StorageBackendResult<()> {
            self.inner.clear()
        }

        fn for_each_fields_multi_ref_with_presence(
            &self,
            doc_ids: &[DocId],
            fields: &[&str],
            visitor: &mut dyn FnMut(DocId, bool, &[&uqa_core::Value]) -> bool,
        ) -> StorageBackendResult<()> {
            self.projections
                .lock()
                .push(fields.iter().map(|field| (*field).to_string()).collect());
            self.document_batches.lock().push(doc_ids.to_vec());
            self.inner
                .for_each_fields_multi_ref_with_presence(doc_ids, fields, visitor)
        }

        fn doc_ids(&self) -> StorageBackendResult<Vec<DocId>> {
            self.inner.doc_ids()
        }

        fn len(&self) -> StorageBackendResult<usize> {
            self.inner.len()
        }

        fn snapshot(&self) -> StorageBackendResult<Arc<dyn DocumentStore>> {
            self.inner.snapshot()
        }

        fn writable_snapshot(&self) -> StorageBackendResult<Box<dyn DocumentStore>> {
            self.inner.writable_snapshot()
        }
    }

    fn install_projection_recorder(
        engine: &Engine,
        table_name: &str,
    ) -> (ProjectionLog, DocumentBatchLog) {
        let table = engine.require_table(table_name).unwrap();
        let projections = Arc::new(Mutex::new(Vec::new()));
        let document_batches = Arc::new(Mutex::new(Vec::new()));
        let mut store = table.document_store.write();
        let inner = std::mem::replace(&mut *store, Box::new(MemoryDocumentStore::new()));
        *store = Box::new(ProjectionRecordingStore {
            inner,
            projections: Arc::clone(&projections),
            document_batches: Arc::clone(&document_batches),
        });
        (projections, document_batches)
    }

    fn assert_no_retrieval_fields(projections: &[Vec<String>]) {
        assert!(
            !projections.is_empty(),
            "query must fetch its projected rows"
        );
        for projection in projections {
            assert!(!projection.iter().any(|field| field == "body"));
            assert!(!projection.iter().any(|field| field == "embedding"));
        }
    }

    #[test]
    fn accelerated_retrieval_materializes_only_relational_dependencies() {
        let directory = tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("projection.sqlite3")).unwrap();
        engine
            .sql(
                "CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(2), marker TEXT)",
                &[],
            )
            .unwrap();
        engine
            .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, body, embedding, marker) VALUES \
                 (1, 'alpha beta', ARRAY[1.0, 0.0], 'keep'), \
                 (2, 'alpha gamma', ARRAY[0.9, 0.1], 'keep'), \
                 (3, 'delta', ARRAY[0.0, 1.0], 'drop')",
                &[],
            )
            .unwrap();
        let (projections, document_batches) = install_projection_recorder(&engine, "docs");
        let hybrid = "pool_positive_evidence(\
            bayesian_match(body, 'alpha'), \
            calibrated_vector_match(embedding, ARRAY[1.0, 0.0], 3), \
            alpha => 0.5)";

        let result = engine
            .sql(
                &format!(
                    "SELECT id, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 2"
                ),
                &[],
            )
            .unwrap();
        assert_eq!(result.rows.len(), 2);
        {
            let recorded = projections.lock();
            assert_no_retrieval_fields(&recorded);
            assert!(recorded
                .iter()
                .all(|projection| projection == &["id".to_string()]));
        }
        assert_eq!(
            document_batches.lock().iter().map(Vec::len).sum::<usize>(),
            2
        );

        let (_, document_batches) = install_projection_recorder(&engine, "docs");
        let second = engine
            .sql(
                &format!(
                    "SELECT id, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 1 OFFSET 1"
                ),
                &[],
            )
            .unwrap();
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].get("id"), result.rows[1].get("id"));
        assert_eq!(
            document_batches.lock().iter().map(Vec::len).sum::<usize>(),
            2
        );

        let (projections, _) = install_projection_recorder(&engine, "docs");
        let projected = engine
            .sql(
                &format!(
                    "SELECT id, embedding, _score FROM docs WHERE {hybrid} \
                     ORDER BY _score DESC, id ASC LIMIT 2"
                ),
                &[],
            )
            .unwrap();
        assert_eq!(projected.rows.len(), 2);
        {
            let recorded = projections.lock();
            assert!(!recorded.is_empty());
            assert!(recorded.iter().all(|projection| {
                projection.iter().any(|field| field == "id")
                    && projection.iter().any(|field| field == "embedding")
                    && !projection.iter().any(|field| field == "body")
            }));
        }

        let (projections, _) = install_projection_recorder(&engine, "docs");
        let filtered = engine
            .sql("SELECT id FROM docs WHERE marker = 'keep' ORDER BY id", &[])
            .unwrap();
        assert_eq!(filtered.rows.len(), 2);
        {
            let recorded = projections.lock();
            assert!(!recorded.is_empty());
            assert!(recorded.iter().all(|projection| {
                projection.iter().any(|field| field == "id")
                    && projection.iter().any(|field| field == "marker")
            }));
        }

        let (projections, _) = install_projection_recorder(&engine, "docs");
        let facets = engine
            .sql(
                &format!("SELECT uqa_facets(marker) FROM docs WHERE {hybrid}"),
                &[],
            )
            .unwrap();
        assert!(!facets.rows.is_empty());
        let recorded = projections.lock();
        assert_no_retrieval_fields(&recorded);
        assert!(recorded
            .iter()
            .all(|projection| { projection == &["marker".to_string()] }));
    }

    #[test]
    fn score_cutoff_leaves_secondary_ordering_exact_across_ties() {
        let directory = tempdir().unwrap();
        let engine = Engine::open(&directory.path().join("score-ties.sqlite3")).unwrap();
        engine
            .sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT)", &[])
            .unwrap();
        engine
            .sql("CREATE INDEX docs_body_gin ON docs USING gin (body)", &[])
            .unwrap();
        engine
            .sql(
                "INSERT INTO docs (id, body) VALUES \
                 (1, 'alpha'), (2, 'alpha'), (3, 'alpha')",
                &[],
            )
            .unwrap();
        let (_, document_batches) = install_projection_recorder(&engine, "docs");

        let result = engine
            .sql(
                "SELECT id, _score FROM docs WHERE bayesian_match(body, 'alpha') \
                 ORDER BY _score DESC, id DESC LIMIT 2",
                &[],
            )
            .unwrap();

        assert_eq!(
            result
                .rows
                .iter()
                .map(|row| row.get("id"))
                .collect::<Vec<_>>(),
            vec![
                Some(&uqa_core::Value::Int(3)),
                Some(&uqa_core::Value::Int(2))
            ]
        );
        assert_eq!(
            document_batches.lock().iter().map(Vec::len).sum::<usize>(),
            3,
            "all rows tied at the cutoff score must reach secondary ordering"
        );
    }
}
