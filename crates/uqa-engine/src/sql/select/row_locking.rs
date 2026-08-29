//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock target resolution and the physical `LockRows` operator.

use super::{
    bind_source_plan_schema, recheck_storage_names_match, ComputePlan, CteScope, Engine,
    QueryBlockPlan, QueryPlan, RelationalPlan, SQLError, SQLParam, ScalarExpr, SourcePlan, Value,
};
use crate::row_locks::LockAcquire;
use crate::sql::virtual_relation_accepts_row_lock;
use uqa_execution::{
    Batch, ExecResult, PhysicalOperator, PhysicalRow, RowProjectionValue, RowSchema,
};
use uqa_sql::ast::{LockStrength, LockWait, LockingClause, RelationPersistence};

#[derive(Clone, Debug)]
pub(in crate::sql) struct ResolvedRowLock {
    pub qualifier: String,
    pub storage_name: String,
    pub display_name: String,
    pub strength: LockStrength,
    pub wait: LockWait,
    pub identity_source: bool,
}

pub(in crate::sql) fn query_has_row_locks(query: &QueryPlan) -> bool {
    query_plan_has_row_locks(query)
}

/// Acquire `PostgreSQL` `AccessShareLock` equivalents for every concrete table referenced by a query. The locks are transaction-scoped, so a cursor declaration keeps its bound relations alive until commit while ordinary persistent statements keep DDL from changing a source during execution.
pub(in crate::sql) fn lock_query_relations(
    engine: &Engine,
    query: &QueryPlan,
) -> Result<(), SQLError> {
    let mut locked = std::collections::BTreeSet::new();
    let mut visiting_views = std::collections::BTreeSet::new();
    lock_query_plan_relations(
        engine,
        query,
        &std::collections::BTreeSet::new(),
        &mut locked,
        &mut visiting_views,
    )
}

fn lock_query_plan_relations(
    engine: &Engine,
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
        lock_query_plan_relations(
            engine,
            &cte.query,
            &definition_scope,
            locked,
            visiting_views,
        )?;
        visible_ctes.insert(cte.name.clone());
    }
    lock_relational_plan_relations(engine, &query.root, &visible_ctes, locked, visiting_views)
}

fn lock_relational_plan_relations(
    engine: &Engine,
    plan: &RelationalPlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    locked: &mut std::collections::BTreeSet<String>,
    visiting_views: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                lock_source_plan_relations(engine, source, visible_ctes, locked, visiting_views)?;
            }
            for subquery in &block.subqueries {
                lock_query_plan_relations(engine, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            lock_query_plan_relations(engine, left, visible_ctes, locked, visiting_views)?;
            lock_query_plan_relations(engine, right, visible_ctes, locked, visiting_views)?;
            for subquery in subqueries {
                lock_query_plan_relations(engine, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                lock_query_plan_relations(engine, subquery, visible_ctes, locked, visiting_views)?;
            }
            Ok(())
        }
    }
}

fn lock_source_plan_relations(
    engine: &Engine,
    source: &SourcePlan,
    visible_ctes: &std::collections::BTreeSet<String>,
    locked: &mut std::collections::BTreeSet<String>,
    visiting_views: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table {
            name,
            include_descendants,
            ..
        } => {
            if visible_ctes.contains(name) {
                return Ok(());
            }
            if let Some(table) = engine.try_resolve_table_name(name).map_err(|error| {
                SQLError::Internal(format!("resolve query relation `{name}`: {error}"))
            })? {
                for member in engine.hierarchy_scan_tables(&table, *include_descendants)? {
                    if locked.insert(member.clone()) {
                        engine.lock_relation(
                            &member,
                            crate::row_locks::RelationLockMode::AccessShare,
                        )?;
                    }
                }
                return Ok(());
            }
            if let Some(view) = engine.view_plan(name)? {
                let view_name = engine
                    .try_resolve_view_name(name)
                    .map_err(|error| {
                        SQLError::Internal(format!("resolve query view `{name}`: {error}"))
                    })?
                    .unwrap_or_else(|| name.clone());
                if !visiting_views.insert(view_name.clone()) {
                    return Err(SQLError::Internal(format!(
                        "view `{name}` has a recursive relation dependency"
                    )));
                }
                let result = lock_query_plan_relations(
                    engine,
                    &view,
                    &std::collections::BTreeSet::new(),
                    locked,
                    visiting_views,
                );
                visiting_views.remove(&view_name);
                return result;
            }
            if let Some(foreign) = engine.resolve_foreign_table_name(name).map_err(|error| {
                SQLError::Internal(format!("resolve foreign query relation `{name}`: {error}"))
            })? {
                if locked.insert(foreign.clone()) {
                    engine
                        .lock_relation(&foreign, crate::row_locks::RelationLockMode::AccessShare)?;
                }
            }
            Ok(())
        }
        SourcePlan::Join { left, right, .. } => {
            lock_source_plan_relations(engine, left, visible_ctes, locked, visiting_views)?;
            lock_source_plan_relations(engine, right, visible_ctes, locked, visiting_views)
        }
        SourcePlan::Subquery { body, .. } => {
            lock_query_plan_relations(engine, body, visible_ctes, locked, visiting_views)
        }
        SourcePlan::Function { relation, .. } => {
            lock_table_function_relation(engine, relation.as_deref(), locked)
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                lock_table_function_relation(engine, function.relation.as_deref(), locked)?;
            }
            Ok(())
        }
        SourcePlan::Values { .. } => Ok(()),
    }
}

fn lock_table_function_relation(
    engine: &Engine,
    relation: Option<&str>,
    locked: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let Some(relation) = relation else {
        return Ok(());
    };
    let Some(table) = engine.try_resolve_table_name(relation).map_err(|error| {
        SQLError::Internal(format!(
            "resolve table-function relation `{relation}`: {error}"
        ))
    })?
    else {
        return Ok(());
    };
    if locked.insert(table.clone()) {
        engine.lock_relation(&table, crate::row_locks::RelationLockMode::AccessShare)?;
    }
    Ok(())
}

/// Resolve cursor row-lock targets without opening or pulling the query. `PostgreSQL` performs these declaration-time checks even though expression evaluation and tuple locking wait until FETCH.
pub(in crate::sql) fn validate_query_row_locks(
    engine: &Engine,
    query: &QueryPlan,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let ctes = CteScope::new_for_current_routine();
    validate_query_plan_row_locks(engine, query, params, &ctes)
}

fn validate_query_plan_row_locks(
    engine: &Engine,
    query: &QueryPlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    for cte in &query.ctes {
        validate_query_plan_row_locks(engine, &cte.query, params, ctes)?;
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            for subquery in &block.subqueries {
                validate_query_plan_row_locks(engine, subquery, params, ctes)?;
            }
            if let Some(from) = block.from.as_ref() {
                validate_source_row_locks(engine, from, params, ctes)?;
                resolve_row_locks(
                    engine,
                    from,
                    &block.locking,
                    block.r#where.as_ref(),
                    params,
                    ctes,
                )?;
            }
        }
        RelationalPlan::SetOp { left, right, .. } => {
            validate_query_plan_row_locks(engine, left, params, ctes)?;
            validate_query_plan_row_locks(engine, right, params, ctes)?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                validate_query_plan_row_locks(engine, subquery, params, ctes)?;
            }
        }
    }
    Ok(())
}

fn validate_source_row_locks(
    engine: &Engine,
    source: &SourcePlan,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            validate_source_row_locks(engine, left, params, ctes)?;
            validate_source_row_locks(engine, right, params, ctes)
        }
        SourcePlan::Subquery { body, .. } => {
            validate_query_plan_row_locks(engine, body, params, ctes)
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

fn query_plan_has_row_locks(query: &QueryPlan) -> bool {
    query
        .ctes
        .iter()
        .any(|cte| query_plan_has_row_locks(&cte.query))
        || relational_has_row_locks(&query.root)
}

fn relational_has_row_locks(plan: &RelationalPlan) -> bool {
    match plan {
        RelationalPlan::QueryBlock(block) => {
            !block.locking.is_empty()
                || block.from.as_ref().is_some_and(source_plan_has_row_locks)
                || block.subqueries.iter().any(query_plan_has_row_locks)
        }
        RelationalPlan::SetOp { left, right, .. } => {
            query_plan_has_row_locks(left) || query_plan_has_row_locks(right)
        }
        RelationalPlan::Values { .. } => false,
    }
}

fn source_plan_has_row_locks(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join { left, right, .. } => {
            source_plan_has_row_locks(left) || source_plan_has_row_locks(right)
        }
        SourcePlan::Subquery { body, .. } => query_plan_has_row_locks(body),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => false,
    }
}

pub(in crate::sql) fn resolve_row_locks(
    engine: &Engine,
    from: &SourcePlan,
    locking: &[LockingClause],
    predicate: Option<&ScalarExpr>,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<Vec<ResolvedRowLock>, SQLError> {
    if locking.is_empty() {
        return Ok(Vec::new());
    }
    let mut effective_from = from.clone();
    reduce_null_rejected_outer_joins_to_fixpoint(
        engine,
        &mut effective_from,
        predicate,
        params,
        ctes,
    )?;
    for clause in locking {
        if clause
            .relations
            .iter()
            .any(|relation| source_contains_join_alias(&effective_from, relation))
        {
            return Err(SQLError::Unsupported(format!(
                "{} cannot be applied to a join",
                clause.strength.sql_name()
            )));
        }
    }
    let sources = collect_source_leaves(engine, &effective_from, false, ctes)?;
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
        reject_unusable_lock_leaf(engine, source, strength)?;
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
    if engine.current_transaction_is_read_only() && locks_non_temporary_relation(engine, &resolved)?
    {
        return Err(SQLError::Routine {
            sqlstate: "25006".into(),
            message: "cannot execute SELECT in a read-only transaction".into(),
        });
    }
    Ok(resolved)
}

fn locks_non_temporary_relation(
    engine: &Engine,
    locks: &[ResolvedRowLock],
) -> Result<bool, SQLError> {
    for lock in locks {
        let persistence = engine
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
use null_rejection::reduce_null_rejected_outer_joins_to_fixpoint;

fn merge_lock_wait(left: LockWait, right: LockWait) -> LockWait {
    match (left, right) {
        (LockWait::NoWait, _) | (_, LockWait::NoWait) => LockWait::NoWait,
        (LockWait::SkipLocked, _) | (_, LockWait::SkipLocked) => LockWait::SkipLocked,
        (LockWait::Block, LockWait::Block) => LockWait::Block,
    }
}

/// Apply a row mark selected for a stored view to the view plan before execution. Stored view plans are not present when the SQL compiler pushes row marks into derived tables, so runtime expansion must perform the same propagation to ensure an outer `NOWAIT` or `SKIP LOCKED` policy is merged before an inner row mark can block.
pub(in crate::sql) fn apply_propagated_view_lock(plan: &mut QueryPlan, target: &ResolvedRowLock) {
    apply_propagated_lock_to_relational(&mut plan.root, target.strength, target.wait);
}

fn apply_propagated_lock_to_relational(
    plan: &mut RelationalPlan,
    strength: LockStrength,
    wait: LockWait,
) {
    let RelationalPlan::QueryBlock(block) = plan else {
        return;
    };
    block.locking.push(LockingClause {
        strength,
        wait,
        relations: Vec::new(),
    });
    if let Some(source) = block.from.as_mut() {
        apply_propagated_lock_to_subqueries(source, strength, wait);
    }
}

fn apply_propagated_lock_to_subqueries(
    source: &mut SourcePlan,
    strength: LockStrength,
    wait: LockWait,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            apply_propagated_lock_to_subqueries(left, strength, wait);
            apply_propagated_lock_to_subqueries(right, strength, wait);
        }
        SourcePlan::Subquery { body, .. } => {
            apply_propagated_lock_to_relational(&mut body.root, strength, wait);
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}

struct LockLeaf {
    names: Vec<String>,
    qualifier: String,
    storage_name: String,
    display_name: String,
    kind: LockLeafKind,
    nullable: bool,
}

enum LockLeafKind {
    Base,
    View(Box<QueryPlan>),
    Subquery(Box<QueryPlan>),
    Cte,
    Values,
    Function,
    Foreign,
    Virtual { lockable: bool },
}

impl LockLeafKind {
    fn implicitly_lockable(&self) -> bool {
        matches!(
            self,
            Self::Base | Self::View(_) | Self::Subquery(_) | Self::Foreign | Self::Virtual { .. }
        )
    }

    fn carries_row_identity(&self) -> bool {
        matches!(self, Self::Base | Self::View(_) | Self::Subquery(_))
    }

    fn is_identity_source(&self) -> bool {
        matches!(self, Self::View(_) | Self::Subquery(_))
    }
}

fn collect_source_leaves(
    engine: &Engine,
    source: &SourcePlan,
    nullable: bool,
    ctes: &CteScope,
) -> Result<Vec<LockLeaf>, SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            include_descendants,
        } => {
            let visible = alias.as_deref().unwrap_or(qualifier);
            let mut names = vec![visible.to_string()];
            if alias.is_none() {
                push_unique(&mut names, name);
                if let Some((_, local)) = name.rsplit_once('.') {
                    push_unique(&mut names, local);
                }
            }
            let kind = classify_table_leaf(engine, name, ctes)?;
            if matches!(kind, LockLeafKind::Base) {
                return Ok(engine
                    .query_hierarchy_scan_tables(name, *include_descendants)?
                    .into_iter()
                    .map(|storage_name| LockLeaf {
                        names: names.clone(),
                        qualifier: visible.to_string(),
                        storage_name,
                        display_name: visible.to_string(),
                        kind: LockLeafKind::Base,
                        nullable,
                    })
                    .collect());
            }
            Ok(vec![LockLeaf {
                names,
                qualifier: visible.to_string(),
                storage_name: name.clone(),
                display_name: visible.to_string(),
                kind,
                nullable,
            }])
        }
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = match kind {
                uqa_sql::ast::JoinKind::Left => (nullable, true),
                uqa_sql::ast::JoinKind::Right => (true, nullable),
                uqa_sql::ast::JoinKind::Full => (true, true),
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross => {
                    (nullable, nullable)
                }
            };
            let mut leaves = collect_source_leaves(engine, left, left_nullable, ctes)?;
            leaves.extend(collect_source_leaves(engine, right, right_nullable, ctes)?);
            Ok(leaves)
        }
        SourcePlan::Values { alias, .. } => Ok(vec![LockLeaf {
            names: alias.iter().cloned().collect(),
            qualifier: alias.clone().unwrap_or_default(),
            storage_name: String::new(),
            display_name: alias.clone().unwrap_or_else(|| "values".into()),
            kind: LockLeafKind::Values,
            nullable,
        }]),
        SourcePlan::Function {
            name,
            output_name,
            alias,
            ..
        } => {
            let visible = alias.as_deref().unwrap_or(output_name);
            Ok(vec![LockLeaf {
                names: vec![visible.to_string(), output_name.clone(), name.clone()],
                qualifier: visible.to_string(),
                storage_name: String::new(),
                display_name: visible.to_string(),
                kind: LockLeafKind::Function,
                nullable,
            }])
        }
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            let first = functions
                .first()
                .ok_or_else(|| SQLError::Internal("ROWS FROM group has no functions".into()))?;
            let visible = alias.as_deref().unwrap_or(&first.output_name);
            Ok(vec![LockLeaf {
                names: vec![visible.to_string()],
                qualifier: visible.to_string(),
                storage_name: String::new(),
                display_name: visible.to_string(),
                kind: LockLeafKind::Function,
                nullable,
            }])
        }
        SourcePlan::Subquery { body, alias, .. } => {
            let visible = alias.clone().unwrap_or_default();
            Ok(vec![LockLeaf {
                names: if visible.is_empty() {
                    Vec::new()
                } else {
                    vec![visible.clone()]
                },
                qualifier: visible.clone(),
                storage_name: String::new(),
                display_name: if visible.is_empty() {
                    "subquery".into()
                } else {
                    visible
                },
                kind: LockLeafKind::Subquery(body.clone()),
                nullable,
            }])
        }
    }
}

fn collect_source_leaf_plans<'a>(
    source: &'a SourcePlan,
    path: &mut Vec<u8>,
    leaves: &mut Vec<(Vec<u8>, &'a SourcePlan)>,
) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            path.push(0);
            collect_source_leaf_plans(left, path, leaves);
            path.pop();
            path.push(1);
            collect_source_leaf_plans(right, path, leaves);
            path.pop();
        }
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. }
        | SourcePlan::Subquery { .. } => leaves.push((path.clone(), source)),
    }
}

#[cold]
#[inline(never)]
fn copy_recheck_source_row(
    engine: &Engine,
    source: &SourcePlan,
    qualifier: &str,
    candidate_schema: &RowSchema,
    candidate: &PhysicalRow,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(RowSchema, PhysicalRow), SQLError> {
    let (schema, slots) = if qualifier.is_empty() {
        let source_schema = bind_source_plan_schema(engine, source, params, ctes, None)?;
        let slots = source_schema
            .identities()
            .iter()
            .map(|identity| {
                candidate_schema
                    .physical_slot_for_identity(identity)
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "row-lock recheck cannot identify unqualified copy-row column `{}`",
                            identity.column()
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        (
            RowSchema::with_identities(
                source_schema.columns().to_vec(),
                source_schema.identities().to_vec(),
                source_schema.column_types().to_vec(),
            ),
            slots,
        )
    } else {
        let layout = candidate_schema.qualified_star_layout(qualifier);
        let columns = layout
            .iter()
            .map(|(column, _, _)| column.clone())
            .collect::<Vec<_>>();
        let types = layout
            .iter()
            .map(|(_, _, column_type)| column_type.clone())
            .collect::<Vec<_>>();
        let slots = layout
            .into_iter()
            .map(|(_, slot, _)| slot)
            .collect::<Vec<_>>();
        (
            RowSchema::with_qualified_types(qualifier, columns, types),
            slots,
        )
    };
    let row = candidate
        .project_with_values(slots.into_iter().map(RowProjectionValue::InputSlot))
        .without_lock_origins();
    Ok((schema, row))
}

fn classify_table_leaf(
    engine: &Engine,
    name: &str,
    ctes: &CteScope,
) -> Result<LockLeafKind, SQLError> {
    if ctes.is_visible_cte(name) {
        return Ok(LockLeafKind::Cte);
    }
    if let Some(plan) = engine.view_plan(name)? {
        return Ok(LockLeafKind::View(Box::new(plan)));
    }
    if engine
        .foreign_table(name)
        .map_err(SQLError::Unsupported)?
        .is_some()
    {
        return Ok(LockLeafKind::Foreign);
    }
    if engine
        .try_query_table(name)
        .map_err(|error| SQLError::Internal(format!("resolve lock target `{name}`: {error}")))?
        .is_some()
    {
        return Ok(LockLeafKind::Base);
    }
    let lockable = virtual_relation_accepts_row_lock(engine, name).unwrap_or(false);
    Ok(LockLeafKind::Virtual { lockable })
}

fn reject_unusable_lock_leaf(
    engine: &Engine,
    source: &LockLeaf,
    strength: LockStrength,
) -> Result<(), SQLError> {
    match &source.kind {
        LockLeafKind::Values => Ok(()),
        LockLeafKind::Function => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to a function".into(),
        )),
        LockLeafKind::Cte => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to a WITH query",
            strength.sql_name()
        ))),
        LockLeafKind::Foreign => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to foreign table \"{}\"",
            strength.sql_name(),
            source.display_name
        ))),
        LockLeafKind::Virtual { lockable: true } => {
            reject_nullable_lock_source(source.nullable, strength)
        }
        LockLeafKind::Virtual { lockable: false } => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to relation \"{}\"",
            strength.sql_name(),
            source.display_name
        ))),
        LockLeafKind::Base => reject_nullable_lock_source(source.nullable, strength),
        LockLeafKind::View(ref plan) => {
            validate_view_locking(engine, plan, strength, source.nullable)
        }
        LockLeafKind::Subquery(plan) => {
            validate_propagated_query(engine, plan, strength, source.nullable, false)
        }
    }
}

fn reject_nullable_lock_source(nullable: bool, strength: LockStrength) -> Result<(), SQLError> {
    if nullable {
        return Err(SQLError::Unsupported(format!(
            "{} cannot be applied to the nullable side of an outer join",
            strength.sql_name()
        )));
    }
    Ok(())
}

fn validate_view_locking(
    engine: &Engine,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
) -> Result<(), SQLError> {
    validate_propagated_query(engine, plan, strength, nullable, true)
}

fn validate_propagated_query(
    engine: &Engine,
    plan: &QueryPlan,
    strength: LockStrength,
    nullable: bool,
    allow_set_operation_root: bool,
) -> Result<(), SQLError> {
    match &plan.root {
        RelationalPlan::SetOp { .. } if allow_set_operation_root => Ok(()),
        RelationalPlan::SetOp { .. } => Err(SQLError::Unsupported(format!(
            "{} is not allowed with UNION/INTERSECT/EXCEPT",
            strength.sql_name()
        ))),
        RelationalPlan::Values { .. } => Ok(()),
        RelationalPlan::QueryBlock(block) => {
            validate_locking_block_shape(block, strength)?;
            let Some(source) = block.from.as_ref() else {
                return Ok(());
            };
            validate_propagated_source(engine, source, strength, nullable, plan)
        }
    }
}

fn validate_locking_block_shape(
    block: &QueryBlockPlan,
    strength: LockStrength,
) -> Result<(), SQLError> {
    let label = strength.sql_name();
    if block.distinct || !block.distinct_on.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with DISTINCT clause"
        )));
    }
    if !block.group_by.is_empty() || !block.grouping_sets.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with GROUP BY clause"
        )));
    }
    if block.having.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with HAVING clause"
        )));
    }
    if matches!(block.compute, ComputePlan::Window)
        || block
            .order_by
            .iter()
            .any(|ordering| ordering.expr.contains_window())
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with window functions"
        )));
    }
    if matches!(block.compute, ComputePlan::Aggregate) {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with aggregate functions"
        )));
    }
    Ok(())
}

fn validate_propagated_source(
    engine: &Engine,
    source: &SourcePlan,
    strength: LockStrength,
    nullable: bool,
    owner: &QueryPlan,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { name, .. } => {
            if owner.ctes.iter().any(|cte| cte.name == *name) || engine.view_plan(name)?.is_some() {
                return Ok(());
            }
            reject_nullable_lock_source(nullable, strength)
        }
        SourcePlan::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = match kind {
                uqa_sql::ast::JoinKind::Left => (nullable, true),
                uqa_sql::ast::JoinKind::Right => (true, nullable),
                uqa_sql::ast::JoinKind::Full => (true, true),
                uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross => {
                    (nullable, nullable)
                }
            };
            validate_propagated_source(engine, left, strength, left_nullable, owner)?;
            validate_propagated_source(engine, right, strength, right_nullable, owner)
        }
        SourcePlan::Subquery { body, .. } => {
            validate_propagated_query(engine, body, strength, nullable, false)
        }
        SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => Ok(()),
    }
}

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}

/// Everything needed to rebuild the plan below this `LockRows` boundary for a tuple-local recheck. The statement is the query block as it existed before order-set rewrites; the rebuild replays the same construction the original pipeline used, so the recheck output matches the boundary schema.
pub(in crate::sql) struct LockRowsRecheckSource {
    statement: QueryBlockPlan,
    ctes: CteScope,
    ordered: bool,
    projections: Vec<super::PhysicalProjection>,
}

impl LockRowsRecheckSource {
    pub(in crate::sql) fn new(statement: &QueryBlockPlan, ctes: &CteScope, ordered: bool) -> Self {
        Self {
            statement: statement.clone(),
            ctes: ctes.clone(),
            ordered,
            projections: Vec::new(),
        }
    }

    pub(in crate::sql) fn with_projections(
        statement: &QueryBlockPlan,
        ctes: &CteScope,
        ordered: bool,
        projections: Vec<super::PhysicalProjection>,
    ) -> Self {
        Self {
            statement: statement.clone(),
            ctes: ctes.clone(),
            ordered,
            projections,
        }
    }
}

pub(in crate::sql) struct LockRows<'a> {
    input: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    params: &'a [SQLParam],
    targets: Vec<ResolvedRowLock>,
    max_rows: Option<u64>,
    emitted: u64,
    pending_rows: std::vec::IntoIter<PhysicalRow>,
    discard_lock_origins: bool,
    retry_cache: Option<std::sync::Arc<super::RowLockRetryCache>>,
    recheck_source: Option<LockRowsRecheckSource>,
    schema: RowSchema,
    /// Base relations that already hold this statement's `RowShare` lock. A view or derived-table target reveals its base relations only through row origins, so the relation lock is taken on first sight of each.
    relation_locked: std::collections::BTreeSet<std::sync::Arc<str>>,
}

impl<'a> LockRows<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::sql) fn new(
        input: Box<dyn PhysicalOperator + 'a>,
        engine: &'a Engine,
        params: &'a [SQLParam],
        targets: Vec<ResolvedRowLock>,
        max_rows: Option<u64>,
        discard_lock_origins: bool,
        retry_cache: Option<std::sync::Arc<super::RowLockRetryCache>>,
        recheck_source: Option<LockRowsRecheckSource>,
    ) -> Self {
        let schema = input.row_schema().clone();
        Self {
            input,
            engine,
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

impl PhysicalOperator for LockRows<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        match (self.input.estimated_cardinality(), self.max_rows) {
            (Some(input), Some(max_rows)) => Some(input.min(max_rows)),
            (estimate, None) | (None, estimate) => estimate,
        }
    }

    fn output_ordering(&self) -> &[uqa_execution::PhysicalOrder] {
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
            self.engine
                .cancellation_token()
                .check()
                .map_err(SQLError::from)?;
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

pub(in crate::sql) fn attach_lock_rows<'a>(
    engine: &'a Engine,
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope,
    max_rows: Option<u64>,
    recheck_source: Option<LockRowsRecheckSource>,
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
        engine,
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
            engine.lock_relation(
                &target.storage_name,
                crate::row_locks::RelationLockMode::RowShare,
            )?;
        }
    }
    // The recheck context is shared per SQL statement through the engine session, so every locking scope reaches it: top-level queries, DML sources, DML CTEs, CREATE TABLE AS, prepared execution, and EXPLAIN ANALYZE bodies.
    let retry_cache = engine.statement_row_lock_cache()?;
    Ok(Box::new(LockRows::new(
        operator,
        engine,
        params,
        targets,
        max_rows,
        !ctes.lock_identities.retain_after_lock,
        Some(retry_cache),
        recheck_source,
    )))
}
