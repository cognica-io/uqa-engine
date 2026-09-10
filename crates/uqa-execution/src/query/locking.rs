//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock target resolution and the physical `LockRows` operator.

use super::recheck_source::LockRowsRecheckSource;
use crate::catalog::schema::virtual_relation_accepts_row_lock as virtual_row_lockable;
use crate::catalog::{CatalogReadView, RelationNameResolution};
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

/// Acquire `PostgreSQL` `AccessShareLock` equivalents for every concrete table referenced by a query. The locks are transaction-scoped, so a cursor declaration keeps its bound relations alive until commit while ordinary persistent statements keep DDL from changing a source during execution.
pub fn lock_query_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
) -> Result<(), SQLError> {
    let mut locked = std::collections::BTreeSet::new();
    let mut visiting_views = std::collections::BTreeSet::new();
    let transition_relations = context.scopes.transition_relation_names();
    lock_query_plan_relations(
        context,
        query,
        &transition_relations,
        &mut locked,
        &mut visiting_views,
    )
}

fn lock_query_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
    inherited_ctes: &std::collections::BTreeSet<String>,
    locked: &mut std::collections::BTreeSet<String>,
    visiting_views: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let mut visible_ctes = inherited_ctes.clone();
    for cte in &query.ctes {
        let mut definition_scope = visible_ctes.clone();
        if cte.recursive {
            definition_scope.insert(cte.name.clone());
        }
        lock_cte_plan_relations(
            context,
            &cte.body,
            &definition_scope,
            locked,
            visiting_views,
        )?;
        visible_ctes.insert(cte.name.clone());
    }
    lock_relational_plan_relations(
        context,
        &query.root,
        &visible_ctes,
        query.relations_bound,
        locked,
        visiting_views,
    )
}

fn lock_cte_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    body: &uqa_sql::plan::CtePlanBody,
    inherited: &std::collections::BTreeSet<String>,
    locked: &mut std::collections::BTreeSet<String>,
    visiting: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match body {
        uqa_sql::plan::CtePlanBody::Query(query) => {
            lock_query_plan_relations(context, query, inherited, locked, visiting)
        }
        uqa_sql::plan::CtePlanBody::Command(command) => {
            let bound = match command.as_ref() {
                uqa_sql::plan::CommandPlan::Insert(plan) => plan.relations_bound,
                uqa_sql::plan::CommandPlan::Update(plan) => plan.relations_bound,
                uqa_sql::plan::CommandPlan::Delete(plan) => plan.relations_bound,
                _ => false,
            };
            if let Some(target) = command.mutation_target() {
                if let Some((table, _)) = context.catalog.resolve_relation(target, bound)? {
                    context
                        .session
                        .lock_relation(&table, crate::row_locks::RelationLockMode::RowExclusive)?;
                }
            }
            let mut visible = inherited.clone();
            if command.ctes().iter().any(|cte| cte.recursive) {
                visible.extend(command.ctes().iter().map(|cte| cte.name.clone()));
            }
            for cte in command.ctes() {
                lock_cte_plan_relations(context, &cte.body, &visible, locked, visiting)?;
                visible.insert(cte.name.clone());
            }
            for query in command.query_inputs() {
                lock_query_plan_relations(context, query, &visible, locked, visiting)?;
            }
            if let Some(source) = command.source_input() {
                lock_source_plan_relations(context, source, &visible, bound, locked, visiting)?;
            }
            Ok(())
        }
    }
}

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

fn lock_relational_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    plan: &RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    relations_bound: bool,
    locked: &mut std::collections::BTreeSet<String>,
    visiting_views: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                lock_source_plan_relations(
                    context,
                    source,
                    visible_ctes,
                    relations_bound,
                    locked,
                    visiting_views,
                )?;
            }
            for subquery in &block.subqueries {
                lock_query_plan_relations(context, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            lock_query_plan_relations(context, left, visible_ctes, locked, visiting_views)?;
            lock_query_plan_relations(context, right, visible_ctes, locked, visiting_views)?;
            for subquery in subqueries {
                lock_query_plan_relations(context, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                lock_query_plan_relations(context, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
    }
}

fn lock_source_plan_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    source: &SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    relations_bound: bool,
    locked: &mut std::collections::BTreeSet<String>,
    visiting_views: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            include_descendants,
            ..
        } => {
            if uqa_sql::semantics::cte_reference_name(name)
                .is_some_and(|name| visible_ctes.contains(&name))
            {
                return Ok(());
            }
            match context.catalog.resolve_relation(name, relations_bound)? {
                Some((table, "table")) => {
                    for member in context
                        .catalog
                        .hierarchy_scan_tables(&table, *include_descendants)?
                    {
                        if locked.insert(member.clone()) {
                            context.session.lock_relation(
                                &member,
                                crate::row_locks::RelationLockMode::AccessShare,
                            )?;
                        }
                    }
                    Ok(())
                }
                Some((view_name, "view")) => {
                    let view = context.catalog.view_plan(&view_name)?.ok_or_else(|| {
                        SQLError::Internal(format!(
                            "resolved query view `{view_name}` disappeared before locking"
                        ))
                    })?;
                    if !visiting_views.insert(view_name.clone()) {
                        return Err(SQLError::Internal(format!(
                            "view `{view_name}` has a recursive relation dependency"
                        )));
                    }
                    let result = lock_query_plan_relations(
                        context,
                        &view,
                        &std::collections::BTreeSet::new(),
                        locked,
                        visiting_views,
                    );
                    visiting_views.remove(&view_name);
                    result
                }
                Some((foreign, "foreign table")) => {
                    if locked.insert(foreign.clone()) {
                        context.session.lock_relation(
                            &foreign,
                            crate::row_locks::RelationLockMode::AccessShare,
                        )?;
                    }
                    Ok(())
                }
                Some(_) | None => Ok(()),
            }
        }
        SourcePlan::Join { left, right, .. } => {
            lock_source_plan_relations(
                context,
                left,
                visible_ctes,
                relations_bound,
                locked,
                visiting_views,
            )?;
            lock_source_plan_relations(
                context,
                right,
                visible_ctes,
                relations_bound,
                locked,
                visiting_views,
            )
        }
        SourcePlan::Subquery { body, .. } => {
            lock_query_plan_relations(context, body, visible_ctes, locked, visiting_views)
        }
        SourcePlan::Function { relations, .. } => {
            lock_table_function_relations(context, relations.as_ref(), relations_bound, locked)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                lock_table_function_relations(
                    context,
                    function.relations.as_ref(),
                    relations_bound,
                    locked,
                )?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

/// Resolve cursor row-lock targets without opening or pulling the query. `PostgreSQL` performs these declaration-time checks even though expression evaluation and tuple locking wait until FETCH.
pub fn validate_query_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    query: &QueryPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctes = context.scopes.current_routine_scope();
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
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

use uqa_sql::semantics::locking::query_plan_has_row_locks;

pub fn resolve_row_locks<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    from: &SourcePlan,
    locking: &[LockingClause],
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<Vec<ResolvedRowLock>, SQLError> {
    if locking.is_empty() {
        return Ok(Vec::new());
    }
    let mut effective_from = from.clone();
    reduce_null_rejected_outer_joins_to_fixpoint(
        context,
        &mut effective_from,
        predicate,
        params,
        ctes,
    )?;
    validate_lock_relation_aliases(&effective_from, locking)?;
    let sources = collect_source_leaves(&effective_from, false, ctes)?;
    let mut assigned: Vec<Option<(LockStrength, LockWait)>> = vec![None; sources.len()];
    for clause in locking {
        let selected = if clause.relations.is_empty() {
            sources
                .iter()
                .enumerate()
                .filter_map(|(index, source)| source.kind.implicitly_lockable().then_some(index))
                .collect::<Vec<_>>()
        } else {
            let mut selected = vec![false; sources.len()];
            for relation in &clause.relations {
                let matches = sources
                    .iter()
                    .enumerate()
                    .filter_map(|(index, source)| {
                        source
                            .names
                            .iter()
                            .any(|name| name == relation)
                            .then_some(index)
                    })
                    .collect::<Vec<_>>();
                if matches.is_empty() {
                    return Err(SQLError::Routine {
                        sqlstate: "42P01".into(),
                        message: format!(
                            "relation \"{relation}\" in FOR UPDATE/SHARE clause not found in FROM clause"
                        ),
                    });
                }
                for source_index in matches {
                    selected[source_index] = true;
                }
            }
            selected
                .into_iter()
                .enumerate()
                .filter_map(|(index, selected)| selected.then_some(index))
                .collect()
        };
        for source_index in selected {
            assigned[source_index] = Some(match assigned[source_index] {
                Some((strength, wait)) => (
                    strength.max(clause.strength),
                    merge_lock_wait(wait, clause.wait),
                ),
                None => (clause.strength, clause.wait),
            });
        }
    }
    let mut resolved = Vec::new();
    for (source, assignment) in sources.iter().zip(assigned) {
        let Some((strength, wait)) = assignment else {
            continue;
        };
        reject_unusable_lock_leaf(context, source, strength)?;
        if !source.kind.carries_row_identity() {
            continue;
        }
        resolved.push(ResolvedRowLock {
            qualifier: source.qualifier.clone(),
            storage_name: source.storage_name.clone(),
            display_name: source.display_name.clone(),
            strength,
            wait,
            identity_source: source.kind.is_identity_source(),
        });
    }
    if context.session.current_transaction_is_read_only()
        && locks_non_temporary_relation(context, &resolved)?
    {
        return Err(SQLError::Routine {
            sqlstate: "25006".into(),
            message: "cannot execute SELECT in a read-only transaction".into(),
        });
    }
    Ok(resolved)
}

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
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => false,
    }
}

mod execution;
mod null_rejection;
mod targets;
use null_rejection::reduce_null_rejected_outer_joins_to_fixpoint;
use targets::lock_table_function_relations;

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
    reject_unusable_lock_leaf, validate_locking_block_shape,
};

pub struct LockRows<'a, S: Clone> {
    input: Box<dyn PhysicalOperator + 'a>,
    context: RowLockContext<'a, S>,
    params: &'a [SQLParam],
    targets: Vec<ResolvedRowLock>,
    max_rows: Option<u64>,
    emitted: u64,
    pending_rows: std::vec::IntoIter<PhysicalRow>,
    discard_lock_origins: bool,
    retry_cache: Option<std::sync::Arc<RowLockRetryCache>>,
    recheck_source: Option<LockRowsRecheckSource<S>>,
    schema: RowSchema,
    /// Base relations that already hold this statement's `RowShare` lock. A view or derived-table target reveals its base relations only through row origins, so the relation lock is taken on first sight of each.
    relation_locked: std::collections::BTreeSet<std::sync::Arc<str>>,
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
        targets: Vec<ResolvedRowLock>,
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
            let Some(batch) = self.input.next()? else {
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
    recheck_source: Option<LockRowsRecheckSource<S>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(first_clause) = statement.locking.first() else {
        return Ok(operator);
    };
    if ctes.row_lock_recheck_active() {
        // A tuple-local recheck re-executes the plan below its own LockRows boundary. Locks for the candidate are already held, so nested locking is suppressed while lock identities keep flowing.
        return Ok(operator);
    }
    validate_locking_block_shape(statement, first_clause.strength)?;
    let Some(from) = statement.from.as_ref() else {
        return Ok(operator);
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
        return Ok(operator);
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
    Ok(Box::new(LockRows::new(
        operator,
        context,
        params,
        targets,
        max_rows,
        !ctes.lock_identities.retain_after_lock,
        Some(retry_cache),
        recheck_source,
    )))
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
