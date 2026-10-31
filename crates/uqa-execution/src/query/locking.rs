//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock target resolution and the physical `LockRows` operator.

use super::recheck_source::{LockRowsRecheckInput, LockRowsRecheckSource};
use crate::query::{binding::bind_source_plan_schema, CteScope};
use crate::row_locks::{
    recheck::recheck_storage_names_match, retry_cache::RowLockRetryCache, LockAcquire,
};
use crate::{Batch, ExecResult, PhysicalOperator, PhysicalRow, RowProjectionValue, RowSchema};
use uqa_sql::ast::{LockStrength, LockWait, LockingClause, RelationPersistence};
use uqa_sql::{
    plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan},
    SQLError, SQLParam, ScalarExpr,
};
pub mod context;
pub use context::RowLockContext;

pub use crate::query::scope::ResolvedRowLock;

pub fn query_has_row_locks(query: &QueryPlan) -> bool {
    query_plan_has_row_locks(query)
}

mod relations;
mod targets;
pub use relations::{lock_explained_relations, lock_query_relations};

fn validate_cte_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    body: &uqa_sql::plan::CtePlanBody,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    match body {
        uqa_sql::plan::CtePlanBody::Query(query) => {
            validate_query_plan_row_locks(context, query, params, ctes)
        }
        uqa_sql::plan::CtePlanBody::Command(command) => {
            for cte in command.ctes() {
                validate_cte_row_locks(context, &cte.body, params, ctes)?;
            }
            for query in command.query_inputs() {
                validate_query_plan_row_locks(context, query, params, ctes)?;
            }
            if let Some(source) = command.source_input() {
                validate_source_row_locks(context, source, params, ctes)?;
            }
            Ok(())
        }
    }
}

/// Resolve cursor row-lock targets without opening or pulling the query. `PostgreSQL` performs these declaration-time checks even though expression evaluation and tuple locking wait until FETCH.
pub fn validate_query_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctes = context.scopes.current_routine_scope()?;
    validate_query_plan_row_locks(context, query, params, &ctes)
}

fn validate_query_plan_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    for cte in &query.ctes {
        validate_cte_row_locks(context, &cte.body, params, ctes)?;
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            for subquery in &block.subqueries {
                validate_query_plan_row_locks(context, subquery, params, ctes)?;
            }
            if let Some(from) = block.from.as_ref() {
                validate_source_row_locks(context, from, params, ctes)?;
                resolve_row_locks(
                    context,
                    from,
                    &block.locking,
                    block.r#where.as_ref(),
                    params,
                    ctes,
                )?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            validate_query_plan_row_locks(context, left, params, ctes)?;
            validate_query_plan_row_locks(context, right, params, ctes)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                validate_query_plan_row_locks(context, subquery, params, ctes)?;
            }
        }
    }
    Ok(())
}

fn validate_source_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            validate_source_row_locks(context, left, params, ctes)?;
            validate_source_row_locks(context, right, params, ctes)
        }
        SourcePlan::Subquery { body, .. } => {
            validate_query_plan_row_locks(context, body, params, ctes)
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

use uqa_sql::semantics::locking::query_plan_has_row_locks;

mod resolution;
pub use resolution::resolve_row_locks;

fn locks_non_temporary_relation<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    locks: &[ResolvedRowLock],
) -> Result<bool, SQLError> {
    for lock in locks {
        let persistence = context
            .catalog
            .table_persistence(&lock.storage_name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "resolve row-lock target `{}`: {error}",
                    lock.storage_name
                ))
            })?;
        if persistence != Some(RelationPersistence::Temporary) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn source_contains_join_alias(source: &SourcePlan, target: &str) -> bool {
    match source {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            alias.as_deref() == Some(target)
                || source_contains_join_alias(left, target)
                || source_contains_join_alias(right, target)
        }
        SourcePlan::RetainedRows(_)
        | SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

mod execution;
mod null_rejection;

fn merge_lock_wait(left: LockWait, right: LockWait) -> LockWait {
    match (left, right) {
        (LockWait::NoWait, _) | (_, LockWait::NoWait) => LockWait::NoWait,
        (LockWait::SkipLocked, _) | (_, LockWait::SkipLocked) => LockWait::SkipLocked,
        (LockWait::Block, LockWait::Block) => LockWait::Block,
    }
}

pub use uqa_sql::semantics::locking::apply_propagated_view_lock;

mod leaf_validation;
use leaf_validation::{
    collect_source_leaf_plans, collect_source_leaves, copy_recheck_source_row,
    validate_locking_block_shape,
};

pub struct LockRows<'a, S: Clone> {
    input: Box<dyn PhysicalOperator + 'a>,
    context: RowLockContext<'a, S>,
    params: &'a [SQLParam],
    targets: uqa_core::memory::Produced<Vec<ResolvedRowLock>>,
    max_rows: Option<u64>,
    emitted: u64,
    pending_rows: std::vec::IntoIter<PhysicalRow>,
    discard_lock_origins: bool,
    retry_cache: Option<std::sync::Arc<RowLockRetryCache>>,
    recheck_source: Option<LockRowsRecheckSource<S>>,
    schema: RowSchema,
    /// Base relations that already hold this statement's `RowShare` lock. A view or derived-table target reveals its base relations only through row origins, so the relation lock is taken on first sight of each.
    relation_locked: std::collections::BTreeSet<crate::RowLockName>,
}

impl<'a, S: Clone> LockRows<'a, S> {
    #[expect(
        clippy::too_many_arguments,
        reason = "keeps execution context inputs aligned"
    )]
    pub fn new(
        input: Box<dyn PhysicalOperator + 'a>,
        context: RowLockContext<'a, S>,
        params: &'a [SQLParam],
        targets: uqa_core::memory::Produced<Vec<ResolvedRowLock>>,
        max_rows: Option<u64>,
        discard_lock_origins: bool,
        retry_cache: Option<std::sync::Arc<RowLockRetryCache>>,
        recheck_source: Option<LockRowsRecheckSource<S>>,
    ) -> Self {
        let schema = input.row_schema().clone();
        Self {
            input,
            context,
            params,
            targets,
            max_rows,
            emitted: 0,
            pending_rows: Vec::new().into_iter(),
            discard_lock_origins,
            retry_cache,
            recheck_source,
            schema,
            relation_locked: std::collections::BTreeSet::new(),
        }
    }
}

impl<S: Clone + Send + Sync + 'static> PhysicalOperator for LockRows<'_, S> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        match (self.input.estimated_cardinality(), self.max_rows) {
            (Some(input), Some(max_rows)) => Some(input.min(max_rows)),
            (estimate, None) | (None, estimate) => estimate,
        }
    }

    fn output_ordering(&self) -> &[crate::PhysicalOrder] {
        self.input.output_ordering()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.emitted = 0;
        self.pending_rows = Vec::new().into_iter();
        self.input.open()
    }

    // Keep the virtual pull boundary intact under `ThinLTO`; the acquisition and recheck state machines below are deliberately separate optimized functions.
    #[inline(never)]
    fn next(&mut self) -> ExecResult<Option<Batch>> {
        if self
            .max_rows
            .is_some_and(|max_rows| self.emitted >= max_rows)
        {
            return Ok(None);
        }
        loop {
            self.context.cancellation.check().map_err(SQLError::from)?;
            if let Some(row) = self.pending_rows.next() {
                if let Some(mut row) = self.lock_physical_row(row)? {
                    if self.discard_lock_origins {
                        row.discard_lock_origins_mut();
                    }
                    self.emitted = self.emitted.saturating_add(1);
                    // One row per batch keeps locking demand-driven: an enclosing consumer such as an outer LIMIT over a locking derived table stops pulling after the rows it needs, so rows it never consumes are never locked (PostgreSQL 18 LockRows semantics). Batching ahead would lock rows the consumer discards.
                    return Ok(Some(Batch::from_physical_rows(
                        self.schema.clone(),
                        vec![row],
                    )));
                }
                continue;
            }
            let Some(batch) = self.input.next_with_demand(crate::physical::demand::ONE)? else {
                return Ok(None);
            };
            self.pending_rows = batch.rows.into_iter();
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input.close()
    }
}

pub fn attach_lock_rows<'a, S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    max_rows: Option<u64>,
    recheck_source: Option<LockRowsRecheckInput<'_, S>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    crate::physical::construction::OperatorBuild::build(operator, |operator| {
        let Some(first_clause) = statement.locking.first() else {
            return Ok(());
        };
        if ctes.row_lock_recheck_active() {
            // A tuple-local recheck re-executes the plan below its own LockRows boundary. Locks for the candidate are already held, so nested locking is suppressed while lock identities keep flowing.
            return Ok(());
        }
        validate_locking_block_shape(statement, first_clause.strength)?;
        let Some(from) = statement.from.as_ref() else {
            return Ok(());
        };
        let targets = resolve_row_locks(
            context,
            from,
            &statement.locking,
            statement.r#where.as_ref(),
            params,
            ctes,
        )?;
        if targets.is_empty() {
            return Ok(());
        }
        let mut locked_relations = std::collections::BTreeSet::new();
        for target in targets.iter().filter(|target| !target.identity_source) {
            if locked_relations.insert(target.storage_name.clone()) {
                context.session.lock_relation(
                    &target.storage_name,
                    crate::row_locks::RelationLockMode::RowShare,
                )?;
            }
        }
        // The recheck context is shared per SQL statement through the context session, so every locking scope reaches it: top-level queries, DML sources, DML CTEs, CREATE TABLE AS, prepared execution, and EXPLAIN ANALYZE bodies.
        let retry_cache = context.session.statement_row_lock_cache()?;
        let recheck_source = recheck_source
            .map(LockRowsRecheckInput::retain)
            .transpose()?;
        operator.wrap(|input| {
            Box::new(LockRows::new(
                input,
                context,
                params,
                targets,
                max_rows,
                !ctes.lock_identities.retain_after_lock,
                Some(retry_cache),
                recheck_source,
            ))
        });
        Ok(())
    })
}

fn validate_lock_relation_aliases(
    from: &SourcePlan,
    locking: &[LockingClause],
) -> Result<(), SQLError> {
    for clause in locking {
        if clause
            .relations
            .iter()
            .any(|relation| source_contains_join_alias(from, relation))
        {
            return Err(SQLError::Unsupported(format!(
                "{} cannot be applied to a join",
                clause.strength.sql_name()
            )));
        }
    }
    Ok(())
}
