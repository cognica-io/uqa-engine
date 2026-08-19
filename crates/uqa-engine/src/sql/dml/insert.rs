//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! INSERT execution, defaults, constraint checks, and vector collection.

use super::{
    apply_prepared_document_rewrite, build_returning_row, coerce_to_column_type,
    decode_prepared_insert_conflict, dml_returning_result, dml_storage_error, doc_id_value,
    document_supplied_id, encode_prepared_insert_conflict, eval_lowered_expression,
    eval_mutation_assignment, index_vectors_for_type, insert_identity_columns,
    integer_primary_key_doc_id, lock_document_key_dependencies,
    lock_existing_document_foreign_key_dependencies, prebuild_locking_returning_row,
    resolve_insert_conflict, returning_has_row_locks, validate_document_constraints,
    validate_document_non_key_constraints, validate_key_constraints, validate_mutation_columns,
    validate_returning_alias_relations, BTreeMap, ColumnType, ConflictActionPlan, ConflictPlan,
    CteScope, DmlReturningShape, DocId, Document, Engine, InsertConflictLocks,
    InsertConflictResolution, InsertPlan, MutationAssignmentTarget, PreparedInsertConflict,
    ReturningProjectionRow, ReturningRowImage, ReturningRowImages, SQLError, SQLParam, SQLResult,
};

pub(in crate::sql) fn run_insert(
    engine: &Engine,
    stmt: InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    if engine.transaction_depth() != 0 {
        run_insert_inner(engine, &stmt, params)
    } else {
        engine.transaction(move |engine| run_insert_inner(engine, &stmt, params))
    }
}

#[allow(clippy::too_many_lines)]
pub(in crate::sql) fn run_insert_inner(
    engine: &Engine,
    stmt: &InsertPlan,
    params: &[SQLParam],
) -> Result<SQLResult, SQLError> {
    engine.lock_relation(
        &stmt.table,
        crate::row_locks::RelationLockMode::RowExclusive,
    )?;
    validate_returning_alias_relations(&stmt.target_qualifier, &stmt.returning_aliases, None)?;
    let mut scope = CteScope::new();
    crate::sql::select::materialize_plan_ctes(engine, &stmt.ctes, params, &mut scope)?;
    scope.scalar_subqueries.clone_from(&stmt.subqueries);
    let prebuild_locking_returning = returning_has_row_locks(&stmt.returning, &scope)?;
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

    // INSERT ... SELECT: seal the inner SELECT into a positional spill first, then route each physical row through the standard add_document path by output position. The sealed source preserves statement-snapshot behavior while repeated PostgreSQL output labels never cross a named-map boundary.
    if let Some(source) = stmt.source.as_deref() {
        let result = crate::sql::select::execute_query_plan_output(
            engine,
            source,
            params,
            &mut scope,
            crate::sql::select::QueryOutputMode::SharedSpill,
        )?;
        let result_width = result.columns.len();
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
        if result_width > columns.len() || (!implicit_columns && result_width != columns.len()) {
            return Err(SQLError::TypeMismatch(format!(
                "INSERT SELECT width {} != column count {}",
                result_width,
                columns.len()
            )));
        }
        let crate::sql::select::QueryRows::SharedSpill(source_rows) = result.rows else {
            return Err(SQLError::Internal(
                "INSERT SELECT source did not retain its positional spill".into(),
            ));
        };
        let mut affected = 0u64;
        let mut returning_rows = Vec::new();
        let cancel = engine.cancellation_token();
        let prepared_schema = uqa_execution::RowSchema::new(vec![
            "__uqa_insert_document".into(),
            "__uqa_insert_conflict".into(),
        ]);
        let mut prepared_buffer = uqa_execution::SpillBuffer::new(
            crate::sql::select::physical_work_mem_bytes(engine)?.max(1),
        );
        let mut has_prepared_effect = false;
        let mut conflict_locks = InsertConflictLocks::new(engine);
        let source_reader = source_rows
            .read_rows()
            .map_err(crate::sql::select::physical_exec_error)?;
        for source_row in source_reader {
            cancel.check()?;
            let source_row = source_row.map_err(crate::sql::select::physical_exec_error)?;
            let mut document = Document::new();
            let source_row = source_row.view();
            for (idx, col) in columns.iter().take(result_width).enumerate() {
                if crate::sql::generated::generated_column_kind(engine, &stmt.table, col)?.is_some()
                {
                    return Err(SQLError::TypeMismatch(format!(
                        "column `{col}` is a generated column; only DEFAULT may be assigned"
                    )));
                }
                let value = source_row
                    .value_at(idx)
                    .cloned()
                    .unwrap_or(super::Value::Null);
                document.insert(
                    col.clone(),
                    coerce_to_column_type(engine, &stmt.table, col, value)?,
                );
            }
            // Complete each source row exactly once, acquire all tuple
            // dependencies, and retain the document in a bounded spill before
            // writer promotion. This keeps volatile defaults single-evaluation
            // while preventing a later source row from waiting behind a tuple
            // owner after an earlier row has already written.
            apply_missing_column_defaults(engine, &stmt.table, &mut document, params)?;
            crate::sql::generated::refresh_stored_generated_columns(
                engine,
                &stmt.table,
                &mut document,
            )?;
            let insert_identity = prepare_insert_identity(
                engine,
                &stmt.table,
                &id_column,
                auto_id_col.as_deref(),
                &mut document,
            )?;
            lock_existing_document_foreign_key_dependencies(engine, &stmt.table, &document)?;
            let prepared_conflict = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
                conflict_locks.prepare_document(
                    &stmt.table,
                    &stmt.target_qualifier,
                    on_conflict,
                    &document,
                    params,
                    &scope,
                )?
            } else {
                let _key_locks =
                    lock_document_key_dependencies(engine, &stmt.table, &document, None)?;
                PreparedInsertConflict::Unresolved
            };
            let prepared_conflict =
                attach_prepared_insert_identity(prepared_conflict, insert_identity);
            has_prepared_effect |= !matches!(&prepared_conflict, PreparedInsertConflict::Skip);
            prepared_buffer
                .push(uqa_execution::Batch::from_physical_rows(
                    prepared_schema.clone(),
                    vec![uqa_execution::PhysicalRow::from_values(vec![
                        super::Value::Map(document),
                        encode_prepared_insert_conflict(prepared_conflict),
                    ])],
                ))
                .map_err(crate::sql::select::physical_exec_error)?;
        }
        let prepared_rows = prepared_buffer
            .into_shared(prepared_schema)
            .map_err(crate::sql::select::physical_exec_error)?;
        let mut prebuilt_returning_rows = Vec::new();
        if has_prepared_effect {
            if prebuild_locking_returning {
                let preflight_reader = prepared_rows
                    .read_rows()
                    .map_err(crate::sql::select::physical_exec_error)?;
                for prepared_row in preflight_reader {
                    let prepared_row =
                        prepared_row.map_err(crate::sql::select::physical_exec_error)?;
                    let Some(super::Value::Map(document)) =
                        prepared_row.view().value_at(0).cloned()
                    else {
                        return Err(SQLError::Internal(
                            "INSERT SELECT preflight spill lost its document payload".into(),
                        ));
                    };
                    let prepared = decode_prepared_insert_conflict(
                        prepared_row.view().value_at(1).cloned().ok_or_else(|| {
                            SQLError::Internal(
                                "INSERT SELECT preflight spill lost its conflict payload".into(),
                            )
                        })?,
                    )?;
                    if let Some(row) = prebuild_prepared_insert_returning_row(
                        engine, stmt, &document, &prepared, params, &scope,
                    )? {
                        prebuilt_returning_rows.push(row);
                    }
                }
            }
            engine.prepare_explicit_transaction_writer()?;
        }
        drop(conflict_locks);
        let mut prebuilt_returning_rows = prebuilt_returning_rows.into_iter();
        let prepared_reader = prepared_rows
            .read_rows()
            .map_err(crate::sql::select::physical_exec_error)?;
        for prepared_row in prepared_reader {
            cancel.check()?;
            let prepared_row = prepared_row.map_err(crate::sql::select::physical_exec_error)?;
            let Some(super::Value::Map(document)) = prepared_row.view().value_at(0).cloned() else {
                return Err(SQLError::Internal(
                    "INSERT SELECT prepared spill lost its document payload".into(),
                ));
            };
            let prepared_conflict = decode_prepared_insert_conflict(
                prepared_row.view().value_at(1).cloned().ok_or_else(|| {
                    SQLError::Internal(
                        "INSERT SELECT prepared spill lost its conflict payload".into(),
                    )
                })?,
            )?;
            let mut prebuilt_returning_row = if prebuild_locking_returning
                && !matches!(&prepared_conflict, PreparedInsertConflict::Skip)
            {
                Some(prebuilt_returning_rows.next().ok_or_else(|| {
                    SQLError::Internal("INSERT SELECT lost a prebuilt RETURNING row".into())
                })?)
            } else {
                None
            };
            validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
            let (doc_id, _supplied_id) = match prepared_conflict {
                PreparedInsertConflict::Skip => continue,
                PreparedInsertConflict::Updated(mut prepared) => {
                    let old_doc_id = prepared.doc_id;
                    let doc_id = apply_prepared_document_rewrite(engine, &mut prepared, params)?;
                    if !stmt.returning.is_empty() {
                        returning_rows.push(match prebuilt_returning_row.take() {
                            Some(row) => row,
                            None => build_returning_row(
                                engine,
                                ReturningProjectionRow {
                                    table: &stmt.table,
                                    target_qualifier: &stmt.target_qualifier,
                                    images: ReturningRowImages {
                                        old: Some(ReturningRowImage {
                                            doc_id: old_doc_id,
                                            document: &prepared.old_document,
                                        }),
                                        new: Some(ReturningRowImage {
                                            doc_id,
                                            document: &prepared.new_document,
                                        }),
                                    },
                                    aliases: &stmt.returning_aliases,
                                    context: None,
                                },
                                &stmt.returning,
                                params,
                                &scope,
                            )?,
                        });
                    }
                    affected += 1;
                    continue;
                }
                PreparedInsertConflict::Insert { doc_id, supplied } => (doc_id, supplied),
                PreparedInsertConflict::Unresolved => {
                    return Err(SQLError::Internal(
                        "INSERT SELECT reached execution without a prepared document identity"
                            .into(),
                    ));
                }
            };
            if let Some(on_conflict) = stmt.on_conflict.as_ref() {
                match resolve_insert_conflict(
                    engine,
                    &stmt.table,
                    &stmt.target_qualifier,
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
                            returning_rows.push(match prebuilt_returning_row.take() {
                                Some(row) => row,
                                None => build_returning_row(
                                    engine,
                                    ReturningProjectionRow {
                                        table: &stmt.table,
                                        target_qualifier: &stmt.target_qualifier,
                                        images: ReturningRowImages {
                                            old: Some(ReturningRowImage {
                                                doc_id: old_doc_id,
                                                document: &old_document,
                                            }),
                                            new: Some(ReturningRowImage {
                                                doc_id,
                                                document: &document,
                                            }),
                                        },
                                        aliases: &stmt.returning_aliases,
                                        context: None,
                                    },
                                    &stmt.returning,
                                    params,
                                    &scope,
                                )?,
                            });
                        }
                        affected += 1;
                        continue;
                    }
                }
            }

            let document = insert_prepared_document_with_constraints(
                engine,
                &stmt.table,
                doc_id,
                document,
                params,
                false,
            )?;
            if !stmt.returning.is_empty() {
                returning_rows.push(match prebuilt_returning_row.take() {
                    Some(row) => row,
                    None => build_returning_row(
                        engine,
                        ReturningProjectionRow {
                            table: &stmt.table,
                            target_qualifier: &stmt.target_qualifier,
                            images: ReturningRowImages {
                                old: None,
                                new: Some(ReturningRowImage {
                                    doc_id,
                                    document: &document,
                                }),
                            },
                            aliases: &stmt.returning_aliases,
                            context: None,
                        },
                        &stmt.returning,
                        params,
                        &scope,
                    )?,
                });
            }
            affected += 1;
        }
        if !stmt.returning.is_empty() {
            return dml_returning_result(
                engine,
                DmlReturningShape {
                    table: &stmt.table,
                    target_qualifier: &stmt.target_qualifier,
                    aliases: &stmt.returning_aliases,
                    returning: &stmt.returning,
                    params,
                    ctes: &scope,
                    supplemental_schema: None,
                },
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
    // Evaluate every VALUES row into a complete document before writer
    // promotion. A scalar subquery inside VALUES may carry FOR UPDATE, and
    // PostgreSQL 18 lets that subquery's row-lock holder update and commit
    // while this INSERT waits; holding the backend-writer lock during that
    // wait would fabricate a deadlock. Evaluating against the statement
    // snapshot before any row is inserted also matches PostgreSQL, where a
    // VALUES subquery never observes the statement's own earlier inserts.
    let mut documents = Vec::with_capacity(stmt.rows.len());
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
        documents.push(document);
    }
    for document in &mut documents {
        apply_missing_column_defaults(engine, &stmt.table, document, params)?;
        crate::sql::generated::refresh_stored_generated_columns(engine, &stmt.table, document)?;
    }
    let mut conflict_locks = InsertConflictLocks::new(engine);
    let mut prepared_conflicts = Vec::with_capacity(documents.len());
    for document in &mut documents {
        let insert_identity = prepare_insert_identity(
            engine,
            &stmt.table,
            &id_column,
            auto_id_col.as_deref(),
            document,
        )?;
        lock_existing_document_foreign_key_dependencies(engine, &stmt.table, document)?;
        let prepared = if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            conflict_locks.prepare_document(
                &stmt.table,
                &stmt.target_qualifier,
                on_conflict,
                document,
                params,
                &scope,
            )?
        } else {
            let _key_locks = lock_document_key_dependencies(engine, &stmt.table, document, None)?;
            PreparedInsertConflict::Unresolved
        };
        prepared_conflicts.push(attach_prepared_insert_identity(prepared, insert_identity));
    }
    let mut prebuilt_returning_rows = Vec::new();
    if prepared_conflicts
        .iter()
        .any(|prepared| !matches!(prepared, PreparedInsertConflict::Skip))
    {
        if prebuild_locking_returning {
            for (document, prepared) in documents.iter().zip(&prepared_conflicts) {
                if let Some(row) = prebuild_prepared_insert_returning_row(
                    engine, stmt, document, prepared, params, &scope,
                )? {
                    prebuilt_returning_rows.push(row);
                }
            }
        }
        engine.prepare_explicit_transaction_writer()?;
    }
    drop(conflict_locks);
    let mut prebuilt_returning_rows = prebuilt_returning_rows.into_iter();
    for (document, prepared_conflict) in documents.into_iter().zip(prepared_conflicts) {
        cancel.check()?;
        let mut prebuilt_returning_row =
            if prebuild_locking_returning
                && !matches!(&prepared_conflict, PreparedInsertConflict::Skip)
            {
                Some(prebuilt_returning_rows.next().ok_or_else(|| {
                    SQLError::Internal("INSERT lost a prebuilt RETURNING row".into())
                })?)
            } else {
                None
            };
        // Non-key constraints run per row after earlier rows of this
        // statement were inserted, like PostgreSQL's end-of-row FK checks:
        // a later VALUES row may reference an earlier one.
        validate_document_non_key_constraints(engine, &stmt.table, &document, params)?;
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
        let (doc_id, supplied_id) = match prepared_conflict {
            PreparedInsertConflict::Skip => continue,
            PreparedInsertConflict::Updated(mut prepared) => {
                let old_doc_id = prepared.doc_id;
                let doc_id = apply_prepared_document_rewrite(engine, &mut prepared, params)?;
                if !stmt.returning.is_empty() {
                    returning_rows.push(match prebuilt_returning_row.take() {
                        Some(row) => row,
                        None => build_returning_row(
                            engine,
                            ReturningProjectionRow {
                                table: &stmt.table,
                                target_qualifier: &stmt.target_qualifier,
                                images: ReturningRowImages {
                                    old: Some(ReturningRowImage {
                                        doc_id: old_doc_id,
                                        document: &prepared.old_document,
                                    }),
                                    new: Some(ReturningRowImage {
                                        doc_id,
                                        document: &prepared.new_document,
                                    }),
                                },
                                aliases: &stmt.returning_aliases,
                                context: None,
                            },
                            &stmt.returning,
                            params,
                            &scope,
                        )?,
                    });
                }
                affected += 1;
                continue;
            }
            PreparedInsertConflict::Insert { doc_id, supplied } => (doc_id, supplied),
            PreparedInsertConflict::Unresolved => {
                return Err(SQLError::Internal(
                    "INSERT reached execution without a prepared document identity".into(),
                ));
            }
        };
        if let Some(on_conflict) = stmt.on_conflict.as_ref() {
            match resolve_insert_conflict(
                engine,
                &stmt.table,
                &stmt.target_qualifier,
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
                        returning_rows.push(match prebuilt_returning_row.take() {
                            Some(row) => row,
                            None => build_returning_row(
                                engine,
                                ReturningProjectionRow {
                                    table: &stmt.table,
                                    target_qualifier: &stmt.target_qualifier,
                                    images: ReturningRowImages {
                                        old: Some(ReturningRowImage {
                                            doc_id: old_doc_id,
                                            document: &old_document,
                                        }),
                                        new: Some(ReturningRowImage {
                                            doc_id,
                                            document: &document,
                                        }),
                                    },
                                    aliases: &stmt.returning_aliases,
                                    context: None,
                                },
                                &stmt.returning,
                                params,
                                &scope,
                            )?,
                        });
                    }
                    affected += 1;
                    continue;
                }
            }
        }

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
            returning_rows.push(match prebuilt_returning_row.take() {
                Some(row) => row,
                None => build_returning_row(
                    engine,
                    ReturningProjectionRow {
                        table: &stmt.table,
                        target_qualifier: &stmt.target_qualifier,
                        images: ReturningRowImages {
                            old: None,
                            new: Some(ReturningRowImage {
                                doc_id,
                                document: &document,
                            }),
                        },
                        aliases: &stmt.returning_aliases,
                        context: None,
                    },
                    &stmt.returning,
                    params,
                    &scope,
                )?,
            });
        }
        affected += 1;
    }
    if !stmt.returning.is_empty() {
        return dml_returning_result(
            engine,
            DmlReturningShape {
                table: &stmt.table,
                target_qualifier: &stmt.target_qualifier,
                aliases: &stmt.returning_aliases,
                returning: &stmt.returning,
                params,
                ctes: &scope,
                supplemental_schema: None,
            },
            returning_rows,
            affected,
        );
    }
    Ok(SQLResult::from_affected(affected))
}

fn prepare_insert_identity(
    engine: &Engine,
    table: &str,
    id_column: &str,
    auto_id_column: Option<&str>,
    document: &mut Document,
) -> Result<(DocId, bool), SQLError> {
    let supplied_id = document_supplied_id(document, id_column, auto_id_column == Some(id_column))?;
    let supplied = supplied_id.is_some();
    let doc_id = match supplied_id {
        Some(doc_id) => doc_id,
        None => engine.allocate_next_id(table)?,
    };
    if auto_id_column == Some(id_column) {
        document.insert(id_column.to_string(), doc_id_value(doc_id)?);
    }
    engine
        .advance_next_id(table, doc_id)
        .map_err(|error| dml_storage_error("prepare INSERT identity", error))?;
    Ok((doc_id, supplied))
}

fn attach_prepared_insert_identity(
    prepared: PreparedInsertConflict,
    (doc_id, supplied): (DocId, bool),
) -> PreparedInsertConflict {
    match prepared {
        PreparedInsertConflict::Unresolved => PreparedInsertConflict::Insert { doc_id, supplied },
        resolved => resolved,
    }
}

fn prebuild_prepared_insert_returning_row(
    engine: &Engine,
    stmt: &InsertPlan,
    document: &Document,
    prepared: &PreparedInsertConflict,
    params: &[SQLParam],
    scope: &CteScope,
) -> Result<Option<uqa_execution::OwnedPhysicalRow>, SQLError> {
    let images = match prepared {
        PreparedInsertConflict::Insert { doc_id, .. } => ReturningRowImages {
            old: None,
            new: Some(ReturningRowImage {
                doc_id: *doc_id,
                document,
            }),
        },
        PreparedInsertConflict::Updated(prepared) => ReturningRowImages {
            old: Some(ReturningRowImage {
                doc_id: prepared.doc_id,
                document: &prepared.old_document,
            }),
            new: Some(ReturningRowImage {
                doc_id: integer_primary_key_doc_id(
                    engine,
                    &prepared.table,
                    &prepared.new_document,
                )?
                .unwrap_or(prepared.doc_id),
                document: &prepared.new_document,
            }),
        },
        PreparedInsertConflict::Skip => return Ok(None),
        PreparedInsertConflict::Unresolved => {
            return Err(SQLError::Internal(
                "INSERT RETURNING preflight has no prepared document identity".into(),
            ))
        }
    };
    prebuild_locking_returning_row(
        engine,
        ReturningProjectionRow {
            table: &stmt.table,
            target_qualifier: &stmt.target_qualifier,
            images,
            aliases: &stmt.returning_aliases,
            context: None,
        },
        &stmt.returning,
        params,
        scope,
    )
    .map(Some)
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
    engine.prepare_explicit_transaction_writer()?;
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
