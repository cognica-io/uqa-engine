//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use super::{
    build_returning_row, coerce_to_column_type, dml_returning_result, dml_storage_error,
    doc_id_value, document_supplied_id, eval_lowered_expression, eval_mutation_assignment,
    index_vectors_for_type, insert_identity_columns, resolve_insert_conflict,
    validate_document_constraints, validate_document_non_key_constraints, validate_key_constraints,
    validate_mutation_columns, BTreeMap, ColumnType, ConflictActionPlan, ConflictPlan, CteScope,
    DocId, Document, Engine, InsertConflictResolution, InsertPlan, MutationAssignmentTarget,
    ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult,
};

pub(in crate::sql) fn run_insert(
    engine: &Engine,
    stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.transaction(move |engine| run_insert_inner(engine, &stmt, params))
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    let mut scope = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut scope)?;
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    // Resolve the table's primary-key column name. Auto-increment
    // (SERIAL / BIGSERIAL) wins; otherwise the scalar PRIMARY KEY
    // column wins; otherwise use the conventional legacy `id` slot.
    // Both VALUES and SELECT sources must derive the internal doc id
    // from this same column or later primary-key rewrites can address a
    // different row than the one that was inserted.
    let (auto_id_col, id_column) = insert_identity_columns(engine, &stmt.table, "INSERT")?;
    if let Some(ConflictPlan {
        action: ConflictActionPlan::Update { assignments, .. },
        ..
    }) = stmt.on_conflict.as_ref()
    {
        validate_mutation_columns(
            engine,
            &stmt.table,
            assignments
                .iter()
                .map(|assignment| assignment.column.as_str()),
            "INSERT ON CONFLICT DO UPDATE",
        )?;
    }

    // INSERT ... SELECT: materialise the inner SELECT first, then
    // route each row through the standard add_document path under
    // the named columns.
    if let Some(source) = stmt.source.as_deref() {
        let result =
            crate::sql::select::execute_query_plan_with_ctes(engine, source, params, &mut scope)?;
        let implicit_columns = stmt.columns.is_empty();
        let columns: Vec<String> = if implicit_columns {
            let target_columns = engine
                .try_table_columns(&stmt.table)
                .map_err(|error| dml_storage_error("INSERT SELECT", error))?;
            if target_columns.is_empty() {
                result.columns.clone()
            } else {
                target_columns
            }
        } else {
            stmt.columns.clone()
        };
        validate_mutation_columns(
            engine,
            &stmt.table,
            columns.iter().map(String::as_str),
            "INSERT SELECT",
        )?;
        if result.columns.len() > columns.len()
            || (!implicit_columns && result.columns.len() != columns.len())
        {
            return Err(SQLError::TypeMismatch(format!(
                "INSERT SELECT width {} != column count {}",
                result.columns.len(),
                columns.len()
            )));
        }
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = engine.cancellation_token();
        for source_row in result.rows {
            cancel.check()?;
            let mut document = Document::new();
            for (idx, col) in columns.iter().take(result.columns.len()).enumerate() {
                if crate::sql::generated::generated_column_kind(engine, &stmt.table, col)?.is_some()
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "column `{col}` is a generated column; only DEFAULT may be assigned"
                    )));
                }
                let source_col = &result.columns[idx];
                if let Some(v) = source_row.get(source_col) {
                    document.insert(
                        col.clone(),
                        coerce_to_column_type(engine, &stmt.table, col, v.clone())?,
                    );
                }
            }

            // INSERT ... SELECT must follow the same constraint and
            // conflict path as INSERT ... VALUES.  In particular,
            // defaults participate in conflict-key inference, while
            // non-key constraints are checked before a conflicting row
            // can be rewritten or skipped.
            apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
            crate::sql::generated::refresh_stored_generated_columns(
                engine,
                &stmt.table,
                &mut document,
            )?;
            validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
            if let Some(on_conflict) = stmt.on_conflict.as_ref() {
                match resolve_insert_conflict(
                    engine,
                    &stmt.table,
                    on_conflict,
                    &document,
                    params,
                    &scope,
                )? {
                    InsertConflictResolution::Insert => {}
                    InsertConflictResolution::Skip => continue,
                    InsertConflictResolution::Updated {
                        old_doc_id,
                        doc_id,
                        old_document,
                        document,
                    } => {
                        if !stmt.returning.is_empty() {
                            returning_rows.push(build_returning_row(
                                engine,
                                &stmt.table,
                                ReturningRowImages {
                                    old: Some(ReturningRowImage {
                                        doc_id: old_doc_id,
                                        document: &old_document,
                                    }),
                                    new: Some(ReturningRowImage {
                                        doc_id,
                                        document: &document,
                                    }),
                                },
                                &stmt.returning_aliases,
                                &stmt.returning,
                                params,
                                &scope,
                            )?);
                        }
                        affected += 1;
                        continue;
                    }
                }
            }

            let supplied_id = document_supplied_id(
                &document,
                &id_column,
                auto_id_col.as_deref() == Some(id_column.as_str()),
            )?;
            let doc_id = match supplied_id {
                Some(doc_id) => doc_id,
                None => engine.allocate_next_id(&stmt.table)?,
            };
            if auto_id_col.as_deref() == Some(id_column.as_str()) {
                document.insert(id_column.clone(), doc_id_value(doc_id)?);
            }
            engine
                .advance_next_id(&stmt.table, doc_id)
                .map_err(|err| dml_storage_error("INSERT SELECT", err))?;
            let document = insert_prepared_document_with_constraints(
                engine,
                &stmt.table,
                doc_id,
                document,
                params,
                false,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(build_returning_row(
                    engine,
                    &stmt.table,
                    ReturningRowImages {
                        old: None,
                        new: Some(ReturningRowImage {
                            doc_id,
                            document: &document,
                        }),
                    },
                    &stmt.returning_aliases,
                    &stmt.returning,
                    params,
                    &scope,
                )?);
            }
            affected += 1;
        }
        if !stmt.returning.is_empty() {
            return dml_returning_result(
                engine,
                &stmt.table,
                &stmt.returning,
                returning_rows,
                affected,
            );
        }
        return Ok(SQLResult::from_affected(affected));
    }

    // Whether a user-supplied id value is covered by the strict UNIQUE
    // pre-check below. The legacy bare-`id` fallback maps a plain
    // column onto the doc id without any uniqueness guarantee, so
    // writes through it may replace an existing document.
    let id_column_is_unique_key = engine
        .try_unique_columns(&stmt.table)
        .map_err(|err| dml_storage_error("INSERT", err))?
        .contains(&id_column);

    let implicit_columns = stmt.columns.is_empty();
    let columns: Vec<String> = if implicit_columns {
        // INSERT without explicit column list: project the table schema.
        let cols = engine
            .try_table_columns(&stmt.table)
            .map_err(|error| dml_storage_error("INSERT", error))?;
        if cols.is_empty() {
            return Err(SQLError::Unsupported(
                "INSERT without column list against a table with no schema".into(),
            ));
        }
        cols
    } else {
        stmt.columns.clone()
    };
    validate_mutation_columns(
        engine,
        &stmt.table,
        columns.iter().map(String::as_str),
        "INSERT",
    )?;

    // No explicit id and no auto-increment column: allocate a synthetic
    // u64 doc_id at insert time. Every table has an implicit doc_id even when the
    // schema declares no primary key.

    let mut affected = 0u64;
    let mut returning_rows = Vec::new();
    let cancel = engine.cancellation_token();
    for row in &stmt.rows {
        cancel.check()?;
        if row.len() > columns.len() || (!implicit_columns && row.len() != columns.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "row width {} != column count {}",
                row.len(),
                columns.len()
            )));
        }
        let mut document = Document::new();
        for (i, col) in columns.iter().take(row.len()).enumerate() {
            if let Some(value) = eval_mutation_assignment(
                engine,
                &scope,
                MutationAssignmentTarget {
                    table: &stmt.table,
                    column: col,
                    action: "INSERT",
                },
                &row[i],
                None,
                params,
            )? {
                document.insert(col.clone(), value);
            }
        }

        // Defaults and all non-key constraints are shared with
        // INSERT ... SELECT. Resolve the internal id only afterwards so
        // an integer primary-key DEFAULT cannot diverge from the row's
        // physical document id.
        apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
        crate::sql::generated::refresh_stored_generated_columns(
            engine,
            &stmt.table,
            &mut document,
        )?;
        validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
        let doc_id = document_supplied_id(
            &document,
            &id_column,
            auto_id_col.as_deref() == Some(id_column.as_str()),
        )?;

        // UNIQUE constraint validation -- before any conflict
        // resolution, every UNIQUE / PRIMARY KEY column whose value
        // is non-null must not already exist in another row. The
        // ON CONFLICT branch below intentionally skips this check
        // because that path explicitly chooses a merge action.
        if stmt.on_conflict.is_none() {
            validate_key_constraints(engine, &stmt.table, &document, None)?;
        }

        // ON CONFLICT lookup -- check whether a row with matching
        // conflict-target columns already exists. The conflict
        // columns may include the primary key, so we collect their
        // current values from the row being inserted.
        if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            match resolve_insert_conflict(
                engine,
                &stmt.table,
                on_conflict,
                &document,
                params,
                &scope,
            )? {
                InsertConflictResolution::Insert => {}
                InsertConflictResolution::Skip => continue,
                InsertConflictResolution::Updated {
                    old_doc_id,
                    doc_id,
                    old_document,
                    document,
                } => {
                    if !stmt.returning.is_empty() {
                        returning_rows.push(build_returning_row(
                            engine,
                            &stmt.table,
                            ReturningRowImages {
                                old: Some(ReturningRowImage {
                                    doc_id: old_doc_id,
                                    document: &old_document,
                                }),
                                new: Some(ReturningRowImage {
                                    doc_id,
                                    document: &document,
                                }),
                            },
                            &stmt.returning_aliases,
                            &stmt.returning,
                            params,
                            &scope,
                        )?);
                    }
                    affected += 1;
                    continue;
                }
            }
        }

        let supplied_id = doc_id.is_some();
        let doc_id = if let Some(id) = doc_id {
            id
        } else {
            let id = engine.allocate_next_id(&stmt.table)?;
            // Only stamp the allocated id back onto the document when
            // the primary-key column is auto-increment. For non-auto
            // primary keys (TEXT, UUID, ...) the user-supplied value
            // already lives in `document[id_column]` and must be
            // preserved -- the synthetic u64 stays internal.
            if auto_id_col.as_deref() == Some(id_column.as_str()) {
                document.insert(id_column.clone(), doc_id_value(id)?);
            }
            id
        };
        engine
            .advance_next_id(&stmt.table, doc_id)
            .map_err(|err| dml_storage_error("INSERT", err))?;
        // A document is known new when nothing can have claimed its doc
        // id: an allocator-issued id is fresh by construction, and a
        // user-supplied id is proven absent by the strict UNIQUE
        // pre-check above - which only covers the id column when it is
        // an actual key (not the legacy bare-`id` fallback). ON
        // CONFLICT skips the pre-check, and its target can miss the
        // primary key, so that path keeps the replacement-aware write.
        let known_new = stmt.on_conflict.is_none() && (!supplied_id || id_column_is_unique_key);
        let document = insert_prepared_document_with_constraints(
            engine,
            &stmt.table,
            doc_id,
            document,
            params,
            known_new,
        )?;
        if !stmt.returning.is_empty() {
            returning_rows.push(build_returning_row(
                engine,
                &stmt.table,
                ReturningRowImages {
                    old: None,
                    new: Some(ReturningRowImage {
                        doc_id,
                        document: &document,
                    }),
                },
                &stmt.returning_aliases,
                &stmt.returning,
                params,
                &scope,
            )?);
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            &stmt.table,
            &stmt.returning,
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

/// `known_new` asserts the caller proved `doc_id` absent (fresh
/// allocator id, or a VALUES insert whose strict uniqueness pre-check
/// ran). Paths that can legitimately overwrite an existing document -
/// MERGE inserts and INSERT ... SELECT with caller-supplied ids - must
/// pass `false` so value-index maintenance unindexes the old values.
pub(in crate::sql) fn insert_prepared_document_with_constraints(
    engine: &Engine,
    table: &str,
    doc_id: DocId,
    mut document: Document,
    params: &[SQLParam],
    known_new: bool,
) -> Result<Document, SQLError> {
    apply_missing_column_defaults(engine, table, &mut document, params)?;
    validate_document_constraints(engine, table, &document, params, None)?;
    engine.add_prepared_document_with_vector_values(
        table,
        doc_id,
        document.clone(),
        document_vectors(engine, table, &document)?,
        known_new,
    )?;
    Ok(document)
}

pub(in crate::sql) fn apply_missing_column_defaults(
    engine: &Engine,
    table: &str,
    document: &mut Document,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    for col in engine
        .try_table_columns(table)
        .map_err(|err| dml_storage_error("INSERT defaults", err))?
    {
        if document.contains_key(&col) {
            continue;
        }
        if let Some(default_expr) = engine
            .try_column_default_expr(table, &col)
            .map_err(|err| dml_storage_error("INSERT defaults", err))?
        {
            let value = coerce_to_column_type(
                engine,
                table,
                &col,
                eval_lowered_expression(engine, &default_expr, None, params)?,
            )?;
            document.insert(col, value);
        }
    }
    Ok(())
}

pub(in crate::sql) fn document_vectors(
    engine: &Engine,
    table: &str,
    document: &Document,
) -> Result<BTreeMap<uqa_core::FieldName, Vec<Vec<f32>>>, SQLError> {
    let mut vectors = BTreeMap::new();
    for (field, value) in document {
        let Some(ty) = engine
            .column_type(table, field)
            .map_err(|err| dml_storage_error("vector extraction", err))?
        else {
            continue;
        };
        if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(field.clone(), index_vectors_for_type(value, &ty)?);
        }
    }
    Ok(vectors)
}
