//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `FOR UPDATE` / `FOR SHARE` compilation and `PostgreSQL` 18 validation.

use super::{FromClause, JoinKind, Node, NodeEnum, Result, SQLError, SelectStmt};
use crate::ast::{LockStrength, LockWait, LockingClause};

pub(in crate::compiler) fn compile_locking_clauses(nodes: &[Node]) -> Result<Vec<LockingClause>> {
    nodes.iter().map(compile_locking_clause).collect()
}

fn compile_locking_clause(node: &Node) -> Result<LockingClause> {
    let Some(NodeEnum::LockingClause(clause)) = node.node.as_ref() else {
        return Err(SQLError::Internal(
            "locking clause node is not LockingClause".into(),
        ));
    };
    let strength = match clause.strength() {
        pg_query::protobuf::LockClauseStrength::LcsForupdate => LockStrength::ForUpdate,
        pg_query::protobuf::LockClauseStrength::LcsFornokeyupdate => LockStrength::ForNoKeyUpdate,
        pg_query::protobuf::LockClauseStrength::LcsForshare => LockStrength::ForShare,
        pg_query::protobuf::LockClauseStrength::LcsForkeyshare => LockStrength::ForKeyShare,
        other => {
            return Err(SQLError::Internal(format!(
                "unsupported locking strength {other:?}"
            )));
        }
    };
    let wait = match clause.wait_policy() {
        pg_query::protobuf::LockWaitPolicy::LockWaitSkip => LockWait::SkipLocked,
        pg_query::protobuf::LockWaitPolicy::LockWaitError => LockWait::NoWait,
        pg_query::protobuf::LockWaitPolicy::LockWaitBlock
        | pg_query::protobuf::LockWaitPolicy::Undefined => LockWait::Block,
    };
    let mut relations = Vec::with_capacity(clause.locked_rels.len());
    for relation in &clause.locked_rels {
        let Some(NodeEnum::RangeVar(range)) = relation.node.as_ref() else {
            return Err(SQLError::Internal(
                "FOR UPDATE/SHARE OF target is not a relation".into(),
            ));
        };
        if !range.catalogname.is_empty() || !range.schemaname.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "FOR UPDATE must specify unqualified relation names".into(),
            });
        }
        // OF names identify FROM items by their visible name, exactly as
        // written after identifier processing: an alias, or an unqualified
        // relation name. PostgreSQL compares the raw identifier, so `OF "A"`
        // matches the alias `"A"`; the rendered (quoted) form is not used.
        relations.push(range.relname.clone());
    }
    Ok(LockingClause {
        strength,
        wait,
        relations,
    })
}

pub(in crate::compiler) fn validate_select_locking(statement: &SelectStmt) -> Result<()> {
    if statement.locking.is_empty() {
        return Ok(());
    }
    let label = statement.locking[0].strength.sql_name();
    validate_locking_shape(statement, label)?;
    let Some(from) = statement.from.as_ref() else {
        return Ok(());
    };
    let targets = collect_lock_sources(from, false, &statement.with);
    let defer_nullable_validation = statement.r#where.as_ref().is_some_and(|expression| {
        expression.contains_unqualified_column()
            || expression.contains_function_with_unknown_strictness()
    });
    apply_locking_targets(&statement.locking, &targets, defer_nullable_validation)
}

/// Push a query block's row marks into selected derived tables. `PostgreSQL`
/// merges those pushed-down clauses with row marks written inside the derived
/// query, so the strongest lock and strictest wait policy are applied before
/// either clause can block independently.
pub(in crate::compiler) fn propagate_select_locking(statement: &mut SelectStmt) -> Result<()> {
    let clauses = statement.locking.clone();
    if clauses.is_empty() {
        return Ok(());
    }
    let cte_names = statement
        .with
        .iter()
        .map(|cte| cte.name.clone())
        .collect::<Vec<_>>();
    let Some(from) = statement.from.as_mut() else {
        return Ok(());
    };
    push_clauses_into_selected_subqueries(from, &clauses, &cte_names)
}

fn push_clauses_into_selected_subqueries(
    from: &mut FromClause,
    clauses: &[LockingClause],
    cte_names: &[String],
) -> Result<()> {
    let sources =
        collect_lock_sources_matching(from, false, &|name| cte_names.iter().any(|cte| cte == name));
    let mut pushed = vec![Vec::new(); sources.len()];
    for clause in clauses {
        let selected = selected_source_indexes(clause, &sources)?;
        for source_index in selected {
            if matches!(sources[source_index].kind, LockSourceKind::Subquery(_)) {
                pushed[source_index].push(LockingClause {
                    strength: clause.strength,
                    wait: clause.wait,
                    relations: Vec::new(),
                });
            }
        }
    }
    drop(sources);
    let mut source_index = 0;
    apply_pushed_subquery_clauses(from, &mut source_index, &mut pushed)
}

fn apply_pushed_subquery_clauses(
    from: &mut FromClause,
    source_index: &mut usize,
    pushed: &mut [Vec<LockingClause>],
) -> Result<()> {
    match from {
        FromClause::Join { left, right, .. } => {
            apply_pushed_subquery_clauses(left, source_index, pushed)?;
            apply_pushed_subquery_clauses(right, source_index, pushed)
        }
        FromClause::Subquery { body, .. } => {
            let clauses = std::mem::take(&mut pushed[*source_index]);
            *source_index += 1;
            if clauses.is_empty() {
                return Ok(());
            }
            body.locking.extend(clauses.iter().cloned());
            let child_cte_names = body
                .with
                .iter()
                .map(|cte| cte.name.clone())
                .collect::<Vec<_>>();
            if let Some(child_from) = body.from.as_mut() {
                push_clauses_into_selected_subqueries(child_from, &clauses, &child_cte_names)?;
            }
            Ok(())
        }
        FromClause::Table { .. } | FromClause::Values { .. } | FromClause::Function { .. } => {
            *source_index += 1;
            Ok(())
        }
    }
}

fn validate_locking_shape(statement: &SelectStmt, label: &str) -> Result<()> {
    if statement.set_op.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with UNION/INTERSECT/EXCEPT"
        )));
    }
    if statement.distinct {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with DISTINCT clause"
        )));
    }
    if !statement.group_by.is_empty() || !statement.grouping_sets.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with GROUP BY clause"
        )));
    }
    if statement.having.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with HAVING clause"
        )));
    }
    if statement
        .projections
        .iter()
        .any(|projection| projection.expr.contains_window())
        || statement
            .order_by
            .iter()
            .any(|ordering| ordering.expr.contains_window())
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with window functions"
        )));
    }
    if statement
        .projections
        .iter()
        .any(|projection| projection.expr.contains_aggregate())
        || statement
            .order_by
            .iter()
            .any(|ordering| ordering.expr.contains_aggregate())
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with aggregate functions"
        )));
    }
    if !statement.values.is_empty() {
        return Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to VALUES".into(),
        ));
    }
    Ok(())
}

struct LockSource<'a> {
    names: Vec<String>,
    kind: LockSourceKind<'a>,
    nullable: bool,
}

#[derive(Clone, Copy)]
enum LockSourceKind<'a> {
    Relation,
    Cte,
    Values,
    Function,
    Subquery(&'a SelectStmt),
}

fn collect_lock_sources<'a>(
    from: &'a FromClause,
    nullable: bool,
    ctes: &[crate::ast::CTE],
) -> Vec<LockSource<'a>> {
    collect_lock_sources_matching(from, nullable, &|name| {
        ctes.iter().any(|cte| cte.name == name)
    })
}

fn collect_lock_sources_matching<'a>(
    from: &'a FromClause,
    nullable: bool,
    is_cte: &impl Fn(&str) -> bool,
) -> Vec<LockSource<'a>> {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
        } => {
            if let Some(alias) = alias {
                let mut names = Vec::new();
                push_unique(&mut names, alias);
                return vec![LockSource {
                    names,
                    kind: if is_cte(name) {
                        LockSourceKind::Cte
                    } else {
                        LockSourceKind::Relation
                    },
                    nullable,
                }];
            }
            let mut names = Vec::new();
            push_unique(&mut names, qualifier);
            push_unique(&mut names, name);
            if let Some((_, local)) = name.rsplit_once('.') {
                push_unique(&mut names, local);
            }
            vec![LockSource {
                names,
                kind: if is_cte(name) {
                    LockSourceKind::Cte
                } else {
                    LockSourceKind::Relation
                },
                nullable,
            }]
        }
        FromClause::Join {
            left, right, kind, ..
        } => {
            let (left_nullable, right_nullable) = match kind {
                JoinKind::Left => (nullable, true),
                JoinKind::Right => (true, nullable),
                JoinKind::Full => (true, true),
                JoinKind::Inner | JoinKind::Cross => (nullable, nullable),
            };
            let mut sources = collect_lock_sources_matching(left, left_nullable, is_cte);
            sources.extend(collect_lock_sources_matching(right, right_nullable, is_cte));
            sources
        }
        FromClause::Values { alias, .. } => vec![LockSource {
            names: alias.iter().cloned().collect(),
            kind: LockSourceKind::Values,
            nullable,
        }],
        FromClause::Function {
            name,
            output_name,
            alias,
            ..
        } => {
            let mut names = Vec::new();
            if let Some(alias) = alias {
                push_unique(&mut names, alias);
            } else {
                push_unique(&mut names, output_name);
                push_unique(&mut names, name);
            }
            vec![LockSource {
                names,
                kind: LockSourceKind::Function,
                nullable,
            }]
        }
        FromClause::Subquery { body, alias, .. } => vec![LockSource {
            names: alias.iter().cloned().collect(),
            kind: LockSourceKind::Subquery(body),
            nullable,
        }],
    }
}

fn apply_locking_targets(
    clauses: &[LockingClause],
    sources: &[LockSource<'_>],
    defer_nullable_validation: bool,
) -> Result<()> {
    for clause in clauses {
        let selected = selected_source_indexes(clause, sources)?;
        for source_index in selected {
            reject_unusable_lock_source(&sources[source_index], clause, defer_nullable_validation)?;
        }
    }
    Ok(())
}

fn selected_source_indexes(
    clause: &LockingClause,
    sources: &[LockSource<'_>],
) -> Result<Vec<usize>> {
    if clause.relations.is_empty() {
        return Ok(sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| source.kind.implicitly_lockable().then_some(index))
            .collect());
    }
    let mut selected = vec![false; sources.len()];
    for relation in &clause.relations {
        let matches = matching_sources(sources, relation);
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
    Ok(selected
        .into_iter()
        .enumerate()
        .filter_map(|(index, selected)| selected.then_some(index))
        .collect())
}

fn matching_sources(sources: &[LockSource<'_>], relation: &str) -> Vec<usize> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| {
            source
                .names
                .iter()
                .any(|name| name == relation)
                .then_some(index)
        })
        .collect()
}

fn reject_unusable_lock_source(
    source: &LockSource<'_>,
    clause: &LockingClause,
    defer_nullable_validation: bool,
) -> Result<()> {
    match source.kind {
        LockSourceKind::Cte => Err(SQLError::Unsupported(format!(
            "{} cannot be applied to a WITH query",
            clause.strength.sql_name()
        ))),
        LockSourceKind::Values => Ok(()),
        LockSourceKind::Function => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to a function".into(),
        )),
        LockSourceKind::Relation => {
            if source.nullable && !defer_nullable_validation {
                return Err(SQLError::Unsupported(format!(
                    "{} cannot be applied to the nullable side of an outer join",
                    clause.strength.sql_name()
                )));
            }
            Ok(())
        }
        LockSourceKind::Subquery(statement) => {
            validate_locking_shape(statement, clause.strength.sql_name())?;
            let Some(from) = statement.from.as_ref() else {
                return Ok(());
            };
            let sources = collect_lock_sources(
                from,
                source.nullable && !defer_nullable_validation,
                &statement.with,
            );
            let pushed = LockingClause {
                strength: clause.strength,
                wait: clause.wait,
                relations: Vec::new(),
            };
            apply_locking_targets(&[pushed], &sources, false)
        }
    }
}

impl LockSourceKind<'_> {
    fn implicitly_lockable(self) -> bool {
        matches!(self, Self::Relation | Self::Subquery(_))
    }
}

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}
