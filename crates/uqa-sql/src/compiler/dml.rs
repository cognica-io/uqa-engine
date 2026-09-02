//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! UPDATE and DELETE statement lowering.

use super::{
    compile_expr, compile_from_node, compile_returning_clause, compile_with_clause, range_var_name,
    DeleteStmt, NodeEnum, Result, SQLError, UpdateStmt,
};

pub(super) fn compile_update(stmt: &pg_query::protobuf::UpdateStmt) -> Result<UpdateStmt> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("UPDATE without relation".into()))?;
    let table = range_var_name(relation);
    let target_qualifier = relation
        .alias
        .as_ref()
        .map(|alias| alias.aliasname.as_str())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(&relation.relname)
        .to_string();
    let mut assignments = Vec::new();
    for target_node in &stmt.target_list {
        let inner = target_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("UPDATE contains an empty assignment".into()))?;
        let NodeEnum::ResTarget(rt) = inner else {
            return Err(SQLError::Internal(format!(
                "UPDATE expected ResTarget, got {inner:?}"
            )));
        };
        let value = rt
            .val
            .as_ref()
            .ok_or_else(|| SQLError::Internal("UPDATE assignment without value".into()))?;
        assignments.push((rt.name.clone(), compile_expr(value)?));
    }
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let from = match stmt.from_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(UpdateStmt {
        table,
        target_relation_bound: false,
        target_qualifier,
        include_descendants: relation.inh,
        assignments,
        r#where,
        with,
        from,
        returning,
        returning_aliases,
    })
}

pub(super) fn compile_delete(stmt: &pg_query::protobuf::DeleteStmt) -> Result<DeleteStmt> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("DELETE without relation".into()))?;
    let table = range_var_name(relation);
    let target_qualifier = relation
        .alias
        .as_ref()
        .map(|alias| alias.aliasname.as_str())
        .filter(|alias| !alias.is_empty())
        .unwrap_or(&relation.relname)
        .to_string();
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let using = match stmt.using_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let (returning, returning_aliases) = compile_returning_clause(stmt.returning_clause.as_ref())?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(DeleteStmt {
        table,
        target_relation_bound: false,
        target_qualifier,
        include_descendants: relation.inh,
        r#where,
        with,
        using,
        returning,
        returning_aliases,
    })
}
