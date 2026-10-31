//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    dml_storage_error, lock_physical_mutation_target, missing_document_error, update_lock_strength,
    Document, ForeignKey, ForeignKeyAction, ForeignKeyComparison, PhysicalDocumentIdentity,
    PhysicalMutationLockTarget, ReferentialContext, SQLError, Value,
};
use crate::mutation::candidate_ids::{CandidateIds, CapturedCandidateIds};

/// Completed selection before row effects; consumers refetch and lock each identity independently.
pub struct ReferencingRows {
    tables: Vec<String>,
    rows: Option<CapturedCandidateIds>,
}

impl ReferencingRows {
    pub fn is_empty(&self) -> bool {
        self.rows
            .as_ref()
            .is_none_or(CapturedCandidateIds::is_empty)
    }

    pub fn read_rows(
        &self,
    ) -> Result<impl Iterator<Item = Result<PhysicalDocumentIdentity, SQLError>> + '_, SQLError>
    {
        Ok(self
            .rows
            .as_ref()
            .map(CapturedCandidateIds::read_rows)
            .transpose()?
            .into_iter()
            .flatten()
            .map(|row| {
                let (table, doc_id) = row?;
                let table = self.tables.get(table).ok_or_else(|| {
                    SQLError::Internal("referencing row has an invalid table ordinal".into())
                })?;
                Ok(PhysicalDocumentIdentity {
                    table: table.clone(),
                    doc_id,
                })
            }))
    }
}

/// Lock one referencing child row for a referential action and refetch it after the wait. Returns `None` when the child vanished or its foreign-key columns no longer reference the parent key that triggered the action, so the action skips it exactly like `PostgreSQL` after an `EvalPlanQual` recheck of the referencing row.
pub struct ReferencingChildLock<'a> {
    pub ref_table: &'a str,
    pub child: &'a PhysicalDocumentIdentity,
    pub lock_columns: &'a [String],
    pub foreign_key: &'a ForeignKey,
    pub comparison: &'a ForeignKeyComparison,
    pub expected: &'a [Value],
}

pub fn lock_referencing_child<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    request: ReferencingChildLock<'_>,
) -> Result<Option<(PhysicalDocumentIdentity, Document)>, SQLError> {
    let ReferencingChildLock {
        ref_table,
        child,
        lock_columns,
        foreign_key,
        comparison,
        expected,
    } = request;
    let target = lock_physical_mutation_target(
        context.locking.session,
        &child.table,
        ref_table,
        child.doc_id,
        update_lock_strength(context.locking.catalog, &child.table, lock_columns),
    )?;
    let PhysicalMutationLockTarget::Present { identity, recheck } = target else {
        return Ok(None);
    };
    if recheck {
        context
            .constraints
            .transactions
            .refresh_explicit_statement_snapshot()?;
    }
    let Some(child_doc) = context
        .locking
        .rows
        .get_document_for_mutation(&identity.table, identity.doc_id)?
    else {
        return Ok(None);
    };
    let actual = foreign_key
        .local_columns
        .iter()
        .map(|column| child_doc.get(column).cloned().unwrap_or(Value::Null))
        .collect();
    let actual = comparison.normalize_local(actual)?;
    Ok(
        normalized_keys_equal(context.constraints.values, &actual, expected)?
            .then_some((identity, child_doc)),
    )
}

pub fn referencing_rows<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    fk: &ForeignKey,
    comparison: &ForeignKeyComparison,
    expected: &[Value],
    action: ForeignKeyAction,
) -> Result<ReferencingRows, SQLError> {
    // Referential actions use SQL equality, so a NULL parent key cannot select a child row.
    if expected.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(ReferencingRows {
            tables: Vec::new(),
            rows: None,
        });
    }
    let mut out = CandidateIds::new(
        context.constraints.memory.work_mem_bytes()?,
        context.triggers.runtime.cancellation.clone(),
    );
    let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
    let tables = uqa_sql::semantics::partition::foreign_key_scan_tables(
        context.constraints.partitions.catalog,
        table,
    )?;
    for (table_index, physical_table) in tables.iter().enumerate() {
        let rows = snapshot.table(physical_table)?;
        let indexed = if comparison.exact_local_lookup {
            rows.indexed_doc_ids(
                &fk.local_columns,
                expected,
                context.constraints.memory.work_mem_bytes()?,
                context.triggers.runtime.cancellation,
            )?
        } else {
            None
        };
        let mut visit = |doc_id| -> Result<bool, SQLError> {
            context.triggers.runtime.cancellation.check()?;
            let Some(doc) = rows.document(doc_id)? else {
                return Err(missing_document_error(
                    "foreign-key reference scan",
                    physical_table,
                    doc_id,
                ));
            };
            let values = fk
                .local_columns
                .iter()
                .map(|column| doc.get(column).cloned().unwrap_or(Value::Null))
                .collect();
            if normalized_keys_equal(
                context.constraints.values,
                &comparison.normalize_local(values)?,
                expected,
            )? {
                if matches!(
                    action,
                    ForeignKeyAction::Cascade
                        | ForeignKeyAction::SetNull
                        | ForeignKeyAction::SetDefault
                ) {
                    rows.check_visible(doc_id)?;
                }
                out.push(table_index, doc_id)?;
            }
            Ok(true)
        };
        match indexed {
            Some(ids) => ids.visit(&mut |_, id| visit(id))?,
            None => rows.visit_doc_ids(
                context.constraints.memory.work_mem_bytes()?,
                context.triggers.runtime.cancellation,
                &mut visit,
            )?,
        }
    }
    Ok(ReferencingRows {
        tables,
        rows: Some(out.finish()?),
    })
}

/// Recheck already-coerced FK columns with their SQL field operators, including nested records and arrays.
pub(super) fn normalized_keys_equal(
    catalog: &dyn uqa_sql::expr::SQLValueCatalog,
    actual: &[Value],
    expected: &[Value],
) -> Result<bool, SQLError> {
    if actual.len() != expected.len() {
        return Err(SQLError::Internal(
            "foreign-key comparison widths differ after normalization".into(),
        ));
    }
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    for (actual, expected) in actual.iter().zip(expected) {
        if !uqa_sql::expr::equal_typed_values_with_enum_catalog(
            actual,
            expected,
            &control,
            Some(catalog),
        )? {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn apply_set_action_to_child<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    old_doc: &Document,
    new_doc: &mut Document,
    columns: &[String],
    action: ForeignKeyAction,
) -> Result<(), SQLError> {
    for column in columns {
        let (value, source) = match action {
            ForeignKeyAction::SetNull => (Value::Null, None),
            ForeignKeyAction::SetDefault => {
                if let Some(expr) = context
                    .assignment
                    .columns
                    .try_column_insert_default_expr(table, column)
                    .map_err(|err| dml_storage_error("referential SET DEFAULT", err))?
                {
                    crate::query::catalog_expression::eval_lowered_expression_with_type(
                        context.assignment.expressions.expressions,
                        context.assignment.scopes.current_routine_scope()?,
                        &expr,
                        Some(old_doc),
                        &[],
                    )?
                } else {
                    (Value::Null, None)
                }
            }
            ForeignKeyAction::NoAction | ForeignKeyAction::Restrict | ForeignKeyAction::Cascade => {
                return Err(SQLError::Internal(format!(
                    "invalid SET action helper for `{action:?}`"
                )));
            }
        };
        let value = uqa_sql::assignment::columns::coerce_to_column_type_from(
            context.assignment.assignment,
            context.assignment.columns,
            table,
            column,
            value,
            source.as_ref(),
        )?;
        new_doc.insert(column.clone(), value);
    }
    Ok(())
}
