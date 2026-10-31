//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar mutation qualification retains one projected row and bounded visible-identity pages.

use super::{CteScope, DocId, SQLError, SQLParam, ScalarExpr, SourceContext};
use crate::physical::physical_exec_error;
use crate::{Filter, PhysicalOperator, PhysicalRowIteratorScan};
use std::collections::BTreeSet;

mod rows;

#[cfg(test)]
mod tests;

pub(super) fn visit<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    table: &str,
    qualifier: &str,
    filter: &ScalarExpr,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    visitor: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let source = context.scans.tables.table(table)?;
    let definitions = source.column_definitions();
    let mut requested = BTreeSet::new();
    let projected = filter.collect_columns(&mut requested);
    let tableoid = requested.contains(uqa_sql::semantics::TABLE_OID_COLUMN);
    let stored_id = definitions
        .iter()
        .any(|column| column.name == uqa_sql::semantics::DOC_ID_COLUMN);
    let mut metadata = false;
    filter.visit(&mut |expression| match expression {
        ScalarExpr::QualifiedColumn { qualifier, column }
            if qualifier == uqa_sql::semantics::META_QUALIFIER
                && column == uqa_sql::semantics::META_DOC_ID_COLUMN =>
        {
            metadata = true;
        }
        ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. }
            if !stored_id && column == uqa_sql::semantics::DOC_ID_COLUMN =>
        {
            metadata = true;
        }
        _ => {}
    });
    let columns = if projected {
        requested
            .into_iter()
            .filter(|name| {
                name != uqa_sql::semantics::TABLE_OID_COLUMN
                    && (name != uqa_sql::semantics::DOC_ID_COLUMN || stored_id)
            })
            .collect::<Vec<_>>()
    } else {
        context.text_indexes.column_names(table).map_err(|error| {
            SQLError::Internal(format!("read table columns for `{table}`: {error}"))
        })?
    };
    let mut names = columns.clone();
    let table_oid = tableoid
        .then(|| crate::catalog::projection::table_relation_oid(&context.catalog, table))
        .transpose()?
        .map(uqa_core::Value::Int);
    if tableoid {
        names.push(uqa_sql::semantics::TABLE_OID_COLUMN.into());
    }
    let schema = if metadata {
        super::table_filter_schema(context, table, qualifier, names)?.0
    } else {
        super::table_filter_columns(context, table, qualifier, names)?.0
    };
    let mut candidates = crate::query::key_candidates::key_candidates(
        filter,
        params,
        crate::query::key_candidates::IdentityColumns::new(
            &definitions,
            source.maps_integer_keys(),
            |name| name,
        ),
    )
    .map(|ids| {
        crate::query::SelectedIds::from_sorted(
            ids,
            context.scans.runtime.work_mem_bytes()?,
            context.scans.runtime.cancellation,
        )
    })
    .transpose()?;
    if candidates.is_none() {
        candidates = crate::query::index_candidates::IndexCandidates {
            reads: context.scans.tables,
            table,
            columns: &definitions,
            visible: &str::to_owned,
            params,
            command_visible: ctes.reads_command_overlay(),
            cancellation: context.scans.runtime.cancellation,
            bytes: context.scans.runtime.work_mem_bytes()?,
        }
        .select(filter)?;
    }
    let rows = rows::ScalarRows::new(
        context,
        table,
        qualifier,
        source,
        columns,
        projected,
        metadata,
        table_oid,
        candidates,
        ctes.reads_command_overlay(),
    )?;
    // Preserve WHERE's point-projection read contract. A general borrowed scan may populate a database-wide decoded cache, which is a separate owner from this command's candidate work.
    let scan = PhysicalRowIteratorScan::new(schema, Box::new(rows))
        .with_batch_size(std::num::NonZeroUsize::MIN);
    let (scan, evaluator) =
        crate::physical::construction::OperatorBuild::run(Box::new(scan), |_| {
            context.relational.evaluator(params, ctes)
        })?;
    let mut filtered = Filter::with_evaluator(scan, filter.clone(), evaluator);
    visit_filtered(&mut filtered, visitor)
}

fn visit_filtered(
    operator: &mut dyn PhysicalOperator,
    visitor: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    operator.open().map_err(physical_exec_error)?;
    let result = (|| {
        while let Some(batch) = operator.next().map_err(physical_exec_error)? {
            for row in batch.rows {
                match row.lock_origins() {
                    [origin] => visitor(origin.doc_id)?,
                    _ => {
                        return Err(SQLError::Internal(
                            "physical table filter lost its document identity".into(),
                        ))
                    }
                }
            }
        }
        Ok(())
    })();
    let closed = operator.close().map_err(physical_exec_error);
    result.and(closed)
}
