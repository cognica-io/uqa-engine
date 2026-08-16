//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE source, predicate, and WHEN-clause lowering.

use super::{
    compile_expr, compile_from_node, compile_returning_clause, range_var_name, Expr, NodeEnum,
    Result, SQLError,
};

pub(super) fn compile_merge(stmt: &pg_query::protobuf::MergeStmt) -> Result<crate::ast::MergeStmt> {
    use crate::ast::{MergeStmt, MergeWhen};
    use pg_query::protobuf::{CmdType, MergeMatchKind};
    let target = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("MERGE without target".into()))?;
    let target_alias = stmt
        .relation
        .as_ref()
        .and_then(|r| r.alias.as_ref())
        .map(|a| a.aliasname.clone())
        .filter(|s| !s.is_empty());
    let target_qualifier = target_alias.clone().unwrap_or_else(|| {
        stmt.relation
            .as_ref()
            .map(|relation| relation.relname.clone())
            .unwrap_or_default()
    });
    let source_node = stmt
        .source_relation
        .as_deref()
        .ok_or_else(|| SQLError::Internal("MERGE without USING".into()))?;
    let source = compile_from_node(source_node)?;
    let join_condition_node = stmt
        .join_condition
        .as_deref()
        .ok_or_else(|| SQLError::Internal("MERGE without ON".into()))?;
    let join_condition = compile_expr(join_condition_node)?;

    let mut when_clauses: Vec<MergeWhen> = Vec::with_capacity(stmt.merge_when_clauses.len());
    for clause in &stmt.merge_when_clauses {
        let Some(NodeEnum::MergeWhenClause(w)) = clause.node.as_ref() else {
            return Err(SQLError::Internal(
                "MERGE contains a malformed WHEN clause".into(),
            ));
        };
        let condition = w
            .condition
            .as_deref()
            .map(|c| compile_expr(c))
            .transpose()?;
        let matched = match w.match_kind() {
            MergeMatchKind::MergeWhenMatched => true,
            MergeMatchKind::MergeWhenNotMatchedByTarget => false,
            MergeMatchKind::MergeWhenNotMatchedBySource => {
                return Err(SQLError::Unsupported(
                    "MERGE WHEN NOT MATCHED BY SOURCE is not supported".into(),
                ));
            }
            MergeMatchKind::Undefined => {
                return Err(SQLError::Internal(
                    "MERGE WHEN clause has no match kind".into(),
                ));
            }
        };
        let cmd = w.command_type();
        match cmd {
            CmdType::CmdUpdate => {
                if !matched {
                    return Err(SQLError::Internal(
                        "MERGE UPDATE is only valid for WHEN MATCHED".into(),
                    ));
                }
                let mut assignments: Vec<(String, Expr)> = Vec::new();
                for tgt in &w.target_list {
                    let Some(NodeEnum::ResTarget(rt)) = tgt.node.as_ref() else {
                        return Err(SQLError::Internal(
                            "MERGE UPDATE contains a malformed assignment".into(),
                        ));
                    };
                    let val = rt
                        .val
                        .as_ref()
                        .ok_or_else(|| SQLError::Internal("MERGE UPDATE without value".into()))?;
                    assignments.push((rt.name.clone(), compile_expr(val)?));
                }
                when_clauses.push(MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                });
            }
            CmdType::CmdDelete => {
                if !matched {
                    return Err(SQLError::Internal(
                        "MERGE DELETE is only valid for WHEN MATCHED".into(),
                    ));
                }
                when_clauses.push(MergeWhen::DeleteMatched { condition });
            }
            CmdType::CmdInsert => {
                if matched {
                    return Err(SQLError::Internal(
                        "MERGE INSERT is only valid for WHEN NOT MATCHED".into(),
                    ));
                }
                let mut columns: Vec<String> = Vec::with_capacity(w.target_list.len());
                for tgt in &w.target_list {
                    let Some(NodeEnum::ResTarget(rt)) = tgt.node.as_ref() else {
                        return Err(SQLError::Internal(
                            "MERGE INSERT contains a malformed target column".into(),
                        ));
                    };
                    columns.push(rt.name.clone());
                }
                let values: Vec<Expr> = w
                    .values
                    .iter()
                    .map(compile_expr)
                    .collect::<Result<Vec<_>>>()?;
                when_clauses.push(MergeWhen::InsertNotMatched {
                    condition,
                    columns,
                    values,
                });
            }
            CmdType::CmdNothing => {
                if matched {
                    when_clauses.push(MergeWhen::NothingMatched { condition });
                } else {
                    when_clauses.push(MergeWhen::NothingNotMatched { condition });
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "MERGE WHEN command {other:?}"
                )));
            }
        }
    }

    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    Ok(MergeStmt {
        target,
        target_qualifier,
        target_alias,
        source,
        join_condition,
        when_clauses,
        returning,
        returning_aliases,
    })
}
