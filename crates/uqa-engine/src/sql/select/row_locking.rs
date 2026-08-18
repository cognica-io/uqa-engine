//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-lock target resolution and the physical `LockRows` operator.

use super::{CteScope, Engine, QueryBlockPlan, QueryPlan, RelationalPlan, SQLError, SourcePlan};
use crate::row_locks::LockAcquire;
use uqa_execution::{Batch, ExecResult, PhysicalOperator, PhysicalRow, RowSchema};
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
    ctes: &CteScope,
) -> Result<Vec<ResolvedRowLock>, SQLError> {
    if locking.is_empty() {
        return Ok(Vec::new());
    }
    let sources = collect_source_leaves(engine, from, false, ctes)?;
    let mut assigned = vec![None; sources.len()];
    let mut resolved = Vec::new();
    for (clause_index, clause) in locking.iter().enumerate() {
        let selected = if clause.relations.is_empty() {
            (0..sources.len()).collect::<Vec<_>>()
        } else {
            let mut selected = Vec::new();
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
                match matches.len() {
                    0 => {
                        return Err(SQLError::Routine {
                            sqlstate: "42P01".into(),
                            message: format!(
                                "relation \"{relation}\" in FOR UPDATE/SHARE clause not found in FROM clause"
                            ),
                        });
                    }
                    1 => selected.push(matches[0]),
                    _ => {
                        return Err(SQLError::Routine {
                            sqlstate: "42702".into(),
                            message: format!("column reference \"{relation}\" is ambiguous"),
                        });
                    }
                }
            }
            selected
        };
        for source_index in selected {
            if assigned[source_index].is_some_and(|previous| previous != clause_index) {
                return Err(SQLError::Routine {
                    sqlstate: "42712".into(),
                    message: format!(
                        "multiple FOR UPDATE/SHARE cannot be applied to table \"{}\"",
                        sources[source_index].display_name
                    ),
                });
            }
            assigned[source_index] = Some(clause_index);
            let source = &sources[source_index];
            if source.nullable {
                return Err(SQLError::Unsupported(
                    "FOR UPDATE cannot be applied to the nullable side of an outer join".into(),
                ));
            }
            match source.kind {
                LockLeafKind::Values => {
                    return Err(SQLError::Unsupported(
                        "FOR UPDATE/SHARE cannot be applied to VALUES".into(),
                    ));
                }
                LockLeafKind::Function => {
                    return Err(SQLError::Unsupported(
                        "FOR UPDATE/SHARE cannot be applied to a function".into(),
                    ));
                }
                LockLeafKind::Foreign => {
                    return Err(SQLError::Unsupported(format!(
                        "FOR UPDATE cannot be applied to foreign table \"{}\"",
                        source.display_name
                    )));
                }
                LockLeafKind::Virtual => {
                    return Err(SQLError::Unsupported(format!(
                        "FOR UPDATE cannot be applied to relation \"{}\"",
                        source.display_name
                    )));
                }
                LockLeafKind::Base => {
                    resolved.push(ResolvedRowLock {
                        qualifier: source.qualifier.clone(),
                        storage_name: source.storage_name.clone(),
                        display_name: source.display_name.clone(),
                        strength: clause.strength,
                        wait: clause.wait,
                        identity_source: false,
                    });
                }
                LockLeafKind::Identity => {
                    resolved.extend(expand_identity_locks(engine, source, clause, ctes)?);
                }
            }
        }
    }
    Ok(resolved)
}

struct LockLeaf {
    names: Vec<String>,
    qualifier: String,
    storage_name: String,
    display_name: String,
    kind: LockLeafKind,
    nullable: bool,
}

#[derive(Clone, Copy)]
enum LockLeafKind {
    Base,
    Identity,
    Values,
    Function,
    Foreign,
    Virtual,
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
            push_unique(&mut names, qualifier);
            push_unique(&mut names, name);
            if let Some((_, local)) = name.rsplit_once('.') {
                push_unique(&mut names, local);
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
        SourcePlan::Subquery { alias, .. } => {
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
                kind: LockLeafKind::Identity,
                nullable,
            }])
        }
    }
}

fn expand_identity_locks(
    engine: &Engine,
    source: &LockLeaf,
    clause: &LockingClause,
    ctes: &CteScope,
) -> Result<Vec<ResolvedRowLock>, SQLError> {
    let mut resolved = vec![ResolvedRowLock {
        qualifier: source.qualifier.clone(),
        storage_name: source.storage_name.clone(),
        display_name: source.display_name.clone(),
        strength: clause.strength,
        wait: clause.wait,
        identity_source: true,
    }];
    if let Some(plan) = engine.view_plan(&source.storage_name)? {
        if let RelationalPlan::QueryBlock(block) = &plan.root {
            if let Some(from) = block.from.as_ref() {
                for leaf in collect_source_leaves(engine, from, false, ctes)? {
                    if matches!(leaf.kind, LockLeafKind::Base) {
                        resolved.push(ResolvedRowLock {
                            qualifier: source.qualifier.clone(),
                            storage_name: leaf.storage_name,
                            display_name: source.display_name.clone(),
                            strength: clause.strength,
                            wait: clause.wait,
                            identity_source: false,
                        });
                    }
                }
            }
        }
    }
    Ok(resolved)
}

fn classify_table_leaf(
    engine: &Engine,
    name: &str,
    ctes: &CteScope,
) -> Result<LockLeafKind, SQLError> {
    if ctes.rows.contains_key(name) {
        return Ok(LockLeafKind::Identity);
    }
    if engine.view_plan(name)?.is_some() {
        return Ok(LockLeafKind::Identity);
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

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}

pub(in crate::sql) fn locking_uses_skip(locking: &[LockingClause]) -> bool {
    locking
        .iter()
        .any(|clause| matches!(clause.wait, LockWait::SkipLocked))
}

pub(in crate::sql) struct LockRows<'a> {
    input: Box<dyn PhysicalOperator + 'a>,
    engine: &'a Engine,
    targets: Vec<ResolvedRowLock>,
    schema: RowSchema,
}

impl<'a> LockRows<'a> {
    pub(in crate::sql) fn new(
        input: Box<dyn PhysicalOperator + 'a>,
        engine: &'a Engine,
        targets: Vec<ResolvedRowLock>,
    ) -> Self {
        let schema = input.row_schema().clone();
        Self {
            input,
            engine,
            targets,
            schema,
        }
    }
}

impl PhysicalOperator for LockRows<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.input.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[uqa_execution::PhysicalOrder] {
        self.input.output_ordering()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.input.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        loop {
            let Some(batch) = self.input.next()? else {
                return Ok(None);
            };
            let mut kept = Vec::with_capacity(batch.rows.len());
            for row in batch.rows {
                if let Some(row) =
                    lock_physical_row(self.engine, &self.schema, &row, &self.targets)?
                {
                    kept.push(row);
                }
            }
            if !kept.is_empty() {
                return Ok(Some(Batch::from_physical_rows(self.schema.clone(), kept)));
            }
        }
    }

    fn close(&mut self) -> ExecResult<()> {
        self.input.close()
    }
}

fn lock_physical_row(
    engine: &Engine,
    _schema: &RowSchema,
    row: &PhysicalRow,
    targets: &[ResolvedRowLock],
) -> Result<Option<PhysicalRow>, SQLError> {
    let mut acquired = Vec::new();
    for target in targets {
        for origin in row.lock_origins() {
            if !lock_origin_matches_target(origin, target) {
                continue;
            }
            match engine.lock_row(
                origin.storage_name.as_ref(),
                origin.doc_id,
                target.strength,
                target.wait,
                &target.display_name,
            )? {
                LockAcquire::Granted => {
                    acquired.push((origin.storage_name.to_string(), origin.doc_id, target.wait));
                }
                LockAcquire::Skipped => {
                    for (previous, previous_doc, _) in acquired {
                        engine.unlock_row_acquired_now(&previous, previous_doc);
                    }
                    return Ok(None);
                }
            }
        }
    }
    for (storage_name, doc_id, wait) in &acquired {
        if matches!(wait, LockWait::Block) && engine.get_document(storage_name, *doc_id)?.is_none()
        {
            engine.unlock_row_acquired_now(storage_name, *doc_id);
            return Ok(None);
        }
    }
    Ok(Some(row.clone()))
}

fn lock_origin_matches_target(
    origin: &uqa_execution::RowLockOrigin,
    target: &ResolvedRowLock,
) -> bool {
    if target.identity_source {
        return origin.qualifier.as_ref() == target.qualifier
            || storage_names_match(origin.qualifier.as_ref(), &target.qualifier);
    }
    lock_column_matches_target(
        origin.qualifier.as_ref(),
        origin.storage_name.as_ref(),
        target,
    )
}

pub(in crate::sql) fn lock_column_matches_target(
    qualifier: &str,
    storage: &str,
    target: &ResolvedRowLock,
) -> bool {
    if qualifier == target.qualifier || storage == target.storage_name {
        return true;
    }
    storage_names_match(storage, &target.storage_name)
        || storage_names_match(qualifier, &target.qualifier)
}

fn storage_names_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left_local = left.rsplit_once('.').map_or(left, |(_, local)| local);
    let right_local = right.rsplit_once('.').map_or(right, |(_, local)| local);
    left_local == right_local
}

pub(in crate::sql) fn attach_lock_rows<'a>(
    engine: &'a Engine,
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    ctes: &CteScope,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    let Some(from) = statement.from.as_ref() else {
        return Ok(operator);
    };
    let targets = resolve_row_locks(engine, from, &statement.locking, ctes)?;
    if targets.is_empty() {
        return Ok(operator);
    }
    Ok(Box::new(LockRows::new(operator, engine, targets)))
}
