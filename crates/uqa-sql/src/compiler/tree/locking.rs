//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `FOR UPDATE` / `FOR SHARE` compilation and `PostgreSQL` 18 validation.

use super::{range_var_name, FromClause, JoinKind, Node, NodeEnum, Result, SQLError, SelectStmt};
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
        if !range.catalogname.is_empty() {
            return Err(SQLError::Unsupported(
                "cross-database relation references are not supported".into(),
            ));
        }
        relations.push(range_var_name(range));
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
    {
        return Err(SQLError::Unsupported(format!(
            "{label} is not allowed with window functions"
        )));
    }
    let Some(from) = statement.from.as_ref() else {
        return Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to VALUES".into(),
        ));
    };
    if !statement.values.is_empty() {
        return Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to VALUES".into(),
        ));
    }
    let targets = collect_lock_sources(from, false);
    apply_locking_targets(&statement.locking, &targets)
}

struct LockSource {
    names: Vec<String>,
    kind: LockSourceKind,
    nullable: bool,
}

#[derive(Clone, Copy)]
enum LockSourceKind {
    Relation,
    Values,
    Function,
    Subquery,
}

fn collect_lock_sources(from: &FromClause, nullable: bool) -> Vec<LockSource> {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
        } => {
            let mut names = Vec::new();
            if let Some(alias) = alias {
                push_unique(&mut names, alias);
            }
            push_unique(&mut names, qualifier);
            push_unique(&mut names, name);
            if let Some((_, local)) = name.rsplit_once('.') {
                push_unique(&mut names, local);
            }
            vec![LockSource {
                names,
                kind: LockSourceKind::Relation,
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
            let mut sources = collect_lock_sources(left, left_nullable);
            sources.extend(collect_lock_sources(right, right_nullable));
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
            }
            push_unique(&mut names, output_name);
            push_unique(&mut names, name);
            vec![LockSource {
                names,
                kind: LockSourceKind::Function,
                nullable,
            }]
        }
        FromClause::Subquery { alias, .. } => vec![LockSource {
            names: alias.iter().cloned().collect(),
            kind: LockSourceKind::Subquery,
            nullable,
        }],
    }
}

fn apply_locking_targets(clauses: &[LockingClause], sources: &[LockSource]) -> Result<()> {
    let mut assigned: Vec<Option<usize>> = vec![None; sources.len()];
    for (clause_index, clause) in clauses.iter().enumerate() {
        let selected = if clause.relations.is_empty() {
            (0..sources.len()).collect::<Vec<_>>()
        } else {
            let mut selected = Vec::new();
            for relation in &clause.relations {
                let matches = matching_sources(sources, relation);
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
            if let Some(previous) = assigned[source_index] {
                if previous != clause_index {
                    let name = sources[source_index]
                        .names
                        .first()
                        .map_or("?", String::as_str);
                    return Err(SQLError::Routine {
                        sqlstate: "42712".into(),
                        message: format!(
                            "multiple FOR UPDATE/SHARE cannot be applied to table \"{name}\""
                        ),
                    });
                }
            }
            assigned[source_index] = Some(clause_index);
            reject_unusable_lock_source(&sources[source_index])?;
        }
    }
    Ok(())
}

fn matching_sources(sources: &[LockSource], relation: &str) -> Vec<usize> {
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

fn reject_unusable_lock_source(source: &LockSource) -> Result<()> {
    if source.nullable {
        return Err(SQLError::Unsupported(
            "FOR UPDATE cannot be applied to the nullable side of an outer join".into(),
        ));
    }
    match source.kind {
        LockSourceKind::Values => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to VALUES".into(),
        )),
        LockSourceKind::Function => Err(SQLError::Unsupported(
            "FOR UPDATE/SHARE cannot be applied to a function".into(),
        )),
        LockSourceKind::Relation | LockSourceKind::Subquery => Ok(()),
    }
}

fn push_unique(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|existing| existing == name) {
        names.push(name.to_string());
    }
}
