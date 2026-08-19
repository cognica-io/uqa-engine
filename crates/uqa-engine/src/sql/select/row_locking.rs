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
use uqa_execution::{
    Batch, ExecResult, PhysicalOperator, PhysicalRow, RowProjectionValue, RowSchema,
};
use uqa_sql::ast::{LockStrength, LockWait, LockingClause};

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
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => false,
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
    Ok(resolved)
}

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
        SourcePlan::Table { .. } | SourcePlan::Values { .. } | SourcePlan::Function { .. } => {}
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
    Virtual,
}

impl LockLeafKind {
    fn implicitly_lockable(&self) -> bool {
        matches!(
            self,
            Self::Base | Self::View(_) | Self::Subquery(_) | Self::Foreign | Self::Virtual
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
        .try_table(name)
        .map_err(|error| SQLError::Internal(format!("resolve lock target `{name}`: {error}")))?
        .is_some()
    {
        return Ok(LockLeafKind::Base);
    }
    Ok(LockLeafKind::Virtual)
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
        LockLeafKind::Virtual => Err(SQLError::Unsupported(format!(
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
        SourcePlan::Values { .. } | SourcePlan::Function { .. } => Ok(()),
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
}

impl LockRowsRecheckSource {
    pub(in crate::sql) fn new(statement: &QueryBlockPlan, ctes: &CteScope, ordered: bool) -> Self {
        Self {
            statement: statement.clone(),
            ctes: ctes.clone(),
            ordered,
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

/// One physical tuple this candidate row must lock. A self-join names the same tuple through several visible qualifiers; the tuple is locked once at the strongest requested strength while every qualifier stays known so each marked alias is pinned to the substituted image during a recheck.
struct LockCandidate {
    qualifiers: Vec<std::sync::Arc<str>>,
    /// Base-scan qualifiers that produced this tuple, so identity-source rechecks pin exactly the inner scans that emitted it.
    scan_qualifiers: Vec<std::sync::Arc<str>>,
    storage_name: std::sync::Arc<str>,
    doc_id: uqa_core::DocId,
    display_name: String,
    strength: LockStrength,
    wait: LockWait,
    identity_source: bool,
    foreign_waited: bool,
}

impl LockRows<'_> {
    // Keep candidate acquisition separate from successor traversal so release optimization never merges their control-flow graphs.
    #[inline(never)]
    fn lock_physical_row(&mut self, row: PhysicalRow) -> Result<Option<PhysicalRow>, SQLError> {
        let mut candidates: Vec<LockCandidate> = Vec::new();
        for target in &self.targets {
            for origin in row.lock_origins() {
                if !lock_origin_matches_target(origin, target) {
                    continue;
                }
                if let Some(candidate) = candidates.iter_mut().find(|candidate| {
                    candidate.storage_name == origin.storage_name
                        && candidate.doc_id == origin.doc_id
                }) {
                    candidate.strength = candidate.strength.max(target.strength);
                    candidate.wait = merge_lock_wait(candidate.wait, target.wait);
                    candidate.identity_source |= target.identity_source;
                    if !candidate.qualifiers.contains(&origin.qualifier) {
                        candidate
                            .qualifiers
                            .push(std::sync::Arc::clone(&origin.qualifier));
                    }
                    if !candidate.scan_qualifiers.contains(&origin.scan_qualifier) {
                        candidate
                            .scan_qualifiers
                            .push(std::sync::Arc::clone(&origin.scan_qualifier));
                    }
                    continue;
                }
                candidates.push(LockCandidate {
                    qualifiers: vec![std::sync::Arc::clone(&origin.qualifier)],
                    scan_qualifiers: vec![std::sync::Arc::clone(&origin.scan_qualifier)],
                    storage_name: std::sync::Arc::clone(&origin.storage_name),
                    doc_id: origin.doc_id,
                    display_name: target.display_name.clone(),
                    strength: target.strength,
                    wait: target.wait,
                    identity_source: target.identity_source,
                    foreign_waited: false,
                });
            }
        }

        // PostgreSQL 18 holds RowShare on every base relation whose tuples a locking query returns, including relations reached through views and derived tables, so TRUNCATE and destructive DDL wait for them.
        for candidate in &candidates {
            if !self.relation_locked.contains(&candidate.storage_name) {
                self.engine.lock_relation(
                    candidate.storage_name.as_ref(),
                    crate::row_locks::RelationLockMode::RowShare,
                )?;
                self.relation_locked
                    .insert(std::sync::Arc::clone(&candidate.storage_name));
            }
        }
        let mut acquired = Vec::new();
        let mut waited = false;
        for candidate in &mut candidates {
            match self.engine.lock_row(
                candidate.storage_name.as_ref(),
                candidate.doc_id,
                candidate.strength,
                candidate.wait,
                &candidate.display_name,
            ) {
                Ok(LockAcquire::Granted {
                    waited: lock_waited,
                    foreign_waited,
                    acquisition,
                }) => {
                    waited |= lock_waited;
                    candidate.foreign_waited = foreign_waited;
                    if let Some(acquisition) = acquisition {
                        acquired.push(acquisition);
                    }
                }
                Ok(LockAcquire::Skipped) => {
                    // PostgreSQL retains tuple locks acquired for earlier target relations even when a later target makes this joined row a SKIP LOCKED miss. They remain transaction-scoped just like locks acquired for rows rejected after an EvalPlanQual recheck.
                    return Ok(None);
                }
                Err(error) => {
                    rollback_row_acquisitions(self.engine, acquired);
                    return Err(error);
                }
            }
        }
        // Inside one process, change epochs identify candidates changed after the statement snapshot. Durable candidates also verify their latest committed image unconditionally: an external writer may already have exited by the time this row acquires its lock.
        let foreign_waited = candidates.iter().any(|candidate| candidate.foreign_waited);
        let durable_coordination = self
            .engine
            .row_lock_manager()
            .has_cross_process_coordination();
        let requires_recheck = self.engine.row_lock_change_requires_recheck()?
            || foreign_waited
            || durable_coordination;
        if let Some(cache) = self.retry_cache.as_deref().filter(|_| requires_recheck) {
            return self.recheck_changed_candidates(row, candidates, cache);
        }
        if waited {
            for candidate in &candidates {
                match self
                    .engine
                    .get_document(candidate.storage_name.as_ref(), candidate.doc_id)
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        rollback_row_acquisitions(self.engine, acquired);
                        return Ok(None);
                    }
                    Err(error) => {
                        rollback_row_acquisitions(self.engine, acquired);
                        return Err(error);
                    }
                }
            }
        }
        Ok(Some(row))
    }

    /// `PostgreSQL` 18 `EvalPlanQual`: when a selected tuple was concurrently updated by a committed transaction whose mutation strength conflicts with the requested row lock, re-evaluate this candidate in place. The candidate keeps its original scan and sort position, its original join partners stay pinned, `LIMIT` membership is decided by the recheck outcome, and a primary-key rewrite is followed to the successor row.
    // Keep successor traversal separate from candidate acquisition and committed-image comparison.
    #[inline(never)]
    fn recheck_changed_candidates(
        &self,
        row: PhysicalRow,
        mut candidates: Vec<LockCandidate>,
        cache: &super::RowLockRetryCache,
    ) -> Result<Option<PhysicalRow>, SQLError> {
        let mut overrides: Vec<Option<super::RetryRowOverride>> = Vec::new();
        overrides.resize_with(candidates.len(), || None);
        // Once this session holds a candidate's tuple lock, no other transaction can commit a further conflicting change to it, so a candidate whose committed image was fetched after its lock was acquired is final. Only successor identities surfaced by a primary-key rewrite need another pass: their lock is taken below, after which their refetched image is final too.
        let mut handled = vec![false; candidates.len()];
        let mut visited_doc_ids = candidates
            .iter()
            .map(|candidate| std::collections::BTreeSet::from([candidate.doc_id]))
            .collect::<Vec<_>>();
        let mut any_changed = false;
        // Commits from other OS processes bypass the in-process change epochs, so every durable candidate verifies its latest committed image even after the writer process exits.
        let durable_coordination = self
            .engine
            .row_lock_manager()
            .has_cross_process_coordination();
        loop {
            let mut progressed = false;
            for index in 0..candidates.len() {
                if handled[index] {
                    continue;
                }
                let candidate = &candidates[index];
                let change_target = cache.conflicting_change_target_since_snapshot(
                    candidate.storage_name.as_ref(),
                    candidate.doc_id,
                    candidate.strength,
                )?;
                let target_doc_id = match change_target {
                    crate::row_locks::RowChangeTarget::Deleted => return Ok(None),
                    crate::row_locks::RowChangeTarget::Present(target_doc_id) => target_doc_id,
                    crate::row_locks::RowChangeTarget::Unchanged => {
                        if !(candidate.foreign_waited || durable_coordination) {
                            handled[index] = true;
                            continue;
                        }
                        if self.cross_process_candidate_changed(candidate, cache)? {
                            candidate.doc_id
                        } else {
                            handled[index] = true;
                            continue;
                        }
                    }
                };
                any_changed = true;
                progressed = true;
                let row_override = cache.committed_override(
                    self.engine,
                    candidate.storage_name.as_ref(),
                    candidate.doc_id,
                    target_doc_id,
                    candidate.strength,
                )?;
                match row_override {
                    super::RetryRowOverride::Deleted => {
                        // Lock targets are never on the nullable side of an active outer join, so a deleted tuple always eliminates this candidate row. Locks already acquired stay until transaction end, matching PostgreSQL's treatment of dead EPQ tuples.
                        return Ok(None);
                    }
                    super::RetryRowOverride::Present { doc_id, .. } => {
                        if doc_id == candidate.doc_id {
                            overrides[index] = Some(row_override);
                            handled[index] = true;
                            continue;
                        }
                        if !visited_doc_ids[index].insert(doc_id) {
                            return Err(SQLError::Internal(format!(
                                "row-lock successor chain for relation `{}` contains a cycle at document {doc_id}",
                                candidate.display_name
                            )));
                        }
                        // PostgreSQL follows the update chain to the row the blocker moved the tuple to, locks it, and rechecks that successor. The refetch on the next pass reads the successor after its lock is held, so a change committed while waiting here is still observed.
                        match self.engine.lock_row(
                            candidate.storage_name.as_ref(),
                            doc_id,
                            candidate.strength,
                            candidate.wait,
                            &candidate.display_name,
                        )? {
                            LockAcquire::Granted { .. } => {}
                            LockAcquire::Skipped => {
                                return Ok(None);
                            }
                        }
                        overrides[index] = Some(row_override);
                        candidates[index].doc_id = doc_id;
                    }
                }
            }
            if !progressed {
                break;
            }
        }
        if !any_changed {
            return Ok(Some(row));
        }
        self.run_candidate_recheck(&row, &candidates, &overrides)
    }

    /// Whether another OS process committed a change to this candidate that conflicts with the requested lock strength. Foreign commits are invisible to the in-process change epochs, so the latest committed image is compared with the statement snapshot and the mutation strength is derived from the changed columns, exactly like the epochs derive it from the writer's own column set. A row this transaction already rewrote itself is authoritative as-is.
    // Keep committed-image comparison separate from the successor traversal state machine.
    #[inline(never)]
    fn cross_process_candidate_changed(
        &self,
        candidate: &LockCandidate,
        cache: &super::RowLockRetryCache,
    ) -> Result<bool, SQLError> {
        let table = candidate.storage_name.as_ref();
        if self
            .engine
            .row_changed_in_open_transaction(table, candidate.doc_id)?
        {
            return Ok(false);
        }
        let committed = cache.committed_override(
            self.engine,
            table,
            candidate.doc_id,
            candidate.doc_id,
            candidate.strength,
        )?;
        let committed_document = match &committed {
            // Primary-key rewrites from other processes were already followed through the sidecar journal by the caller, so a missing committed image here is a genuine delete; PostgreSQL 18 drops a candidate whose tuple was deleted.
            super::RetryRowOverride::Deleted => return Ok(true),
            super::RetryRowOverride::Present { document, .. } => document,
        };
        let Some(snapshot_document) = self.engine.get_document(table, candidate.doc_id)? else {
            return Ok(true);
        };
        let mut changed_columns = Vec::new();
        for (column, value) in committed_document {
            if snapshot_document.get(column) != Some(value) {
                changed_columns.push(column.clone());
            }
        }
        for column in snapshot_document.keys() {
            if !committed_document.contains_key(column) {
                changed_columns.push(column.clone());
            }
        }
        if changed_columns.is_empty() {
            return Ok(false);
        }
        let mutation_strength =
            crate::sql::dml::update_lock_strength(self.engine, table, &changed_columns);
        Ok(crate::row_locks::lock_strengths_conflict(
            mutation_strength,
            candidate.strength,
        ))
    }

    /// Re-execute the plan below this `LockRows` boundary with every base scan pinned to the tuple that formed the original candidate. Changed lock targets substitute their committed image; unmarked join partners keep their statement-snapshot image, matching `PostgreSQL`'s `EvalPlanQual` row marks.
    // This contention-only rebuild stays out of the per-row locking path.
    #[cold]
    #[inline(never)]
    fn run_candidate_recheck(
        &self,
        row: &PhysicalRow,
        candidates: &[LockCandidate],
        overrides: &[Option<super::RetryRowOverride>],
    ) -> Result<Option<PhysicalRow>, SQLError> {
        let Some(source) = self.recheck_source.as_ref() else {
            return Err(SQLError::Internal(
                "row-lock recheck attempted without a rebuildable plan source".into(),
            ));
        };
        let source_leaves = source
            .statement
            .from
            .as_ref()
            .map(|from| collect_source_leaves(self.engine, from, false, &source.ctes))
            .transpose()?
            .unwrap_or_default();
        let mut pins = super::RowLockRecheckPins::new();
        if let Some(from) = source.statement.from.as_ref() {
            let mut leaf_plans = Vec::new();
            collect_source_leaf_plans(from, &mut Vec::new(), &mut leaf_plans);
            if leaf_plans.len() != source_leaves.len() {
                return Err(SQLError::Internal(format!(
                    "row-lock recheck found {} source paths for {} source leaves",
                    leaf_plans.len(),
                    source_leaves.len()
                )));
            }
            for ((path, source_plan), leaf) in leaf_plans.into_iter().zip(&source_leaves) {
                let is_lock_target = candidates.iter().any(|candidate| {
                    candidate
                        .qualifiers
                        .iter()
                        .any(|qualifier| qualifier.as_ref() == leaf.qualifier)
                });
                if is_lock_target {
                    continue;
                }
                let (schema, source_row) = copy_recheck_source_row(
                    self.engine,
                    source_plan,
                    &leaf.qualifier,
                    &self.schema,
                    row,
                    self.params,
                    &source.ctes,
                )?;
                pins.pin_source_row(path, leaf.qualifier.clone(), schema, source_row);
            }
        }
        for origin in row.lock_origins() {
            let changed_target = candidates.iter().zip(overrides).find(|(candidate, _)| {
                candidate.storage_name == origin.storage_name
                    && candidate.qualifiers.contains(&origin.qualifier)
                    && candidate.scan_qualifiers.contains(&origin.scan_qualifier)
            });
            let (doc_id, document) =
                changed_target.map_or((origin.doc_id, None), |(candidate, row_override)| {
                    let document = match row_override {
                        Some(super::RetryRowOverride::Present { document, .. }) => {
                            Some(std::sync::Arc::new(document.clone()))
                        }
                        _ => None,
                    };
                    (candidate.doc_id, document)
                });
            let identity_source = source_leaves
                .iter()
                .find(|leaf| leaf.qualifier == origin.qualifier.as_ref())
                .is_some_and(|leaf| leaf.kind.is_identity_source())
                || origin.qualifier != origin.scan_qualifier;
            pins.pin_target(
                origin.qualifier.as_ref(),
                origin.storage_name.as_ref(),
                origin.scan_qualifier.as_ref(),
                identity_source,
                vec![super::RecheckDoc { doc_id, document }],
            );
        }
        let mut recheck_ctes = source.ctes.clone();
        recheck_ctes.activate_row_lock_recheck(std::sync::Arc::new(pins));
        let mut operator = super::build_row_lock_recheck_operator(
            self.engine,
            &source.statement,
            self.params,
            &mut recheck_ctes,
            source.ordered,
        )?;
        operator = align_recheck_schema(operator, &self.schema)?;
        operator.open().map_err(super::physical_exec_error)?;
        let first_row = loop {
            match operator.next() {
                Ok(Some(batch)) => {
                    let schema = batch.schema;
                    if let Some(row) = batch.rows.into_iter().next() {
                        break Some(match schema.relayout_physical_row(row, &self.schema) {
                            Ok(row) => row,
                            Err(error) => {
                                return Err(super::close_after_physical_failure(
                                    operator.as_mut(),
                                    error,
                                    "row-lock recheck relayout",
                                ));
                            }
                        });
                    }
                }
                Ok(None) => break None,
                Err(error) => {
                    let _ = operator.close();
                    return Err(super::physical_exec_error(error));
                }
            }
        };
        operator.close().map_err(super::physical_exec_error)?;
        Ok(first_row)
    }
}

fn rollback_row_acquisitions(
    engine: &Engine,
    acquisitions: Vec<crate::row_locks::RowLockAcquisition>,
) {
    for acquisition in acquisitions.into_iter().rev() {
        engine.rollback_row_lock_acquisition(acquisition);
    }
}

/// Align a rebuilt recheck pipeline with the original lock boundary schema. The single-relation access path derives its scan order from the pruned projection while the join builder uses catalog column order, so the same columns can arrive in a different physical order. Positions are resolved by column identity; any column the rebuild cannot supply is an error, not a silent divergence.
#[cold]
#[inline(never)]
fn align_recheck_schema<'a>(
    operator: Box<dyn PhysicalOperator + 'a>,
    expected: &RowSchema,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let rebuilt = operator.row_schema();
    if rebuilt.columns() == expected.columns() {
        return Ok(operator);
    }
    let mut positions = Vec::with_capacity(expected.len());
    let mut used = vec![false; rebuilt.len()];
    for (index, column) in expected.columns().iter().enumerate() {
        let wanted_identity = expected.identities().get(index);
        let mut resolved = None;
        for (candidate, candidate_column) in rebuilt.columns().iter().enumerate() {
            if used[candidate] || candidate_column != column {
                continue;
            }
            let identity_matches = match (wanted_identity, rebuilt.identities().get(candidate)) {
                (Some(wanted), Some(candidate_identity)) => {
                    wanted.qualifier().is_none()
                        || candidate_identity.qualifier().is_none()
                        || wanted.qualifier() == candidate_identity.qualifier()
                }
                _ => true,
            };
            if !identity_matches {
                continue;
            }
            if resolved.is_some() {
                return Err(SQLError::Internal(format!(
                    "row-lock recheck column `{column}` is ambiguous in the rebuilt schema {:?}",
                    rebuilt.columns()
                )));
            }
            resolved = Some(candidate);
        }
        let Some(position) = resolved else {
            return Err(SQLError::Internal(format!(
                "row-lock recheck schema {:?} diverged from the lock boundary schema {:?}",
                rebuilt.columns(),
                expected.columns()
            )));
        };
        used[position] = true;
        positions.push((column.clone(), position));
    }
    Ok(Box::new(uqa_execution::ColumnSelection::with_positions(
        operator, positions,
    )))
}

fn lock_origin_matches_target(
    origin: &uqa_execution::RowLockOrigin,
    target: &ResolvedRowLock,
) -> bool {
    if origin.qualifier.as_ref() != target.qualifier {
        return false;
    }
    if target.identity_source {
        return true;
    }
    recheck_storage_names_match(origin.storage_name.as_ref(), &target.storage_name)
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
