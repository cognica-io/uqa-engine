//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-body AST lowering helpers for the SQL compiler.

use pg_query::protobuf::Node;
use pg_query::NodeEnum;
use uqa_core::{DecimalValue, Value};

use crate::ast::{
    BinaryOp, ColumnDef, CreateIndex, CreateTable, Expr, FromClause, InsertStmt, JoinKind, OrderBy,
    Projection, SelectStmt, SetOp, SetOpKind, TableKeyConstraint, TableKeyConstraintKind,
    WindowSpec, CTE,
};
use crate::error::{Result, SQLError};

use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, compile_type_name, raw_type_name,
    validate_foreign_key_set_columns,
};
use super::{compile_qualified_name, range_var_name};

pub(super) fn extract_string(node: &Node) -> Result<String> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing string node".into()));
    };
    match inner {
        NodeEnum::String(s) => Ok(s.sval.clone()),
        _ => Err(SQLError::Internal(format!(
            "expected String node, got {inner:?}"
        ))),
    }
}

fn extract_strings(nodes: &[Node]) -> Result<Vec<String>> {
    nodes.iter().map(extract_string).collect()
}

/// Translate a `#>` / `#>>` operator into the argument list of
/// `json_extract_path`. The right-hand side is a Postgres text-array
/// literal like `'{a,b,c}'`; we split it into individual literal
/// segments so the scalar function can walk the path.
fn json_path_args(lhs: Expr, rhs: Expr) -> Vec<Expr> {
    let segments = match &rhs {
        Expr::Literal(uqa_core::Value::Str(s)) => s
            .trim_matches(|c: char| c == '{' || c == '}')
            .split(',')
            .map(|seg| Expr::Literal(uqa_core::Value::Str(seg.trim().to_string())))
            .collect::<Vec<_>>(),
        Expr::Literal(uqa_core::Value::List(items)) => items
            .iter()
            .map(|v| Expr::Literal(v.clone()))
            .collect::<Vec<_>>(),
        _ => vec![rhs],
    };
    let mut out = Vec::with_capacity(segments.len() + 1);
    out.push(lhs);
    out.extend(segments);
    out
}

// -------------------------------------------------------------------------
// CREATE TABLE
// -------------------------------------------------------------------------

pub(super) fn compile_create_table(stmt: &pg_query::protobuf::CreateStmt) -> Result<CreateTable> {
    use crate::ast::{ForeignKey, TableCheck};
    use std::collections::BTreeSet;
    super::validate_create_table_envelope(stmt, "CREATE TABLE")?;
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE without relation".into()))?;
    let name = range_var_name(relation);
    if name.is_empty() {
        return Err(SQLError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    let mut checks: Vec<TableCheck> = Vec::new();
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    let mut key_constraints: Vec<TableKeyConstraint> = Vec::new();
    let mut named_constraints = BTreeSet::new();
    let mut primary_key_seen = false;
    for elt in &stmt.table_elts {
        let inner = elt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE TABLE contains an empty element".into()))?;
        match inner {
            NodeEnum::ColumnDef(col) => {
                for constraint in &col.constraints {
                    let inner = constraint.node.as_ref().ok_or_else(|| {
                        SQLError::Internal("column contains an empty constraint".into())
                    })?;
                    let NodeEnum::Constraint(cstr) = inner else {
                        return Err(SQLError::Internal(format!(
                            "unexpected column constraint node {inner:?}"
                        )));
                    };
                    register_constraint_name(&mut named_constraints, &cstr.conname)?;
                    let kind = match cstr.contype() {
                        pg_query::protobuf::ConstrType::ConstrPrimary => {
                            if primary_key_seen {
                                return Err(SQLError::TypeMismatch(
                                    "multiple PRIMARY KEY constraints are not allowed".into(),
                                ));
                            }
                            primary_key_seen = true;
                            Some(TableKeyConstraintKind::PrimaryKey)
                        }
                        pg_query::protobuf::ConstrType::ConstrUnique => {
                            Some(TableKeyConstraintKind::Unique)
                        }
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        key_constraints.push(TableKeyConstraint {
                            name: constraint_name(&cstr.conname),
                            kind,
                            columns: vec![col.colname.clone()],
                            nulls_not_distinct: cstr.nulls_not_distinct,
                        });
                    }
                }
                columns.push(compile_column_def(col)?);
            }
            NodeEnum::Constraint(cstr) => {
                register_constraint_name(&mut named_constraints, &cstr.conname)?;
                match cstr.contype() {
                    pg_query::protobuf::ConstrType::ConstrCheck => {
                        let raw = cstr
                            .raw_expr
                            .as_deref()
                            .ok_or_else(|| SQLError::Internal("CHECK without expression".into()))?;
                        let expr = compile_expr(raw)?;
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        checks.push(TableCheck { name: cname, expr });
                    }
                    pg_query::protobuf::ConstrType::ConstrForeign => {
                        let local_columns = extract_strings(&cstr.fk_attrs)?;
                        let ref_table =
                            cstr.pktable.as_ref().map(range_var_name).ok_or_else(|| {
                                SQLError::Internal("FOREIGN KEY without referenced table".into())
                            })?;
                        let ref_columns = extract_strings(&cstr.pk_attrs)?;
                        if local_columns.is_empty() || ref_columns.is_empty() {
                            return Err(SQLError::Internal(
                                "FOREIGN KEY without local or referenced columns".into(),
                            ));
                        }
                        if local_columns.len() != ref_columns.len() {
                            return Err(SQLError::TypeMismatch(format!(
                                "FOREIGN KEY has {} local columns but {} referenced columns",
                                local_columns.len(),
                                ref_columns.len()
                            )));
                        }
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        let on_delete_set_columns = extract_strings(&cstr.fk_del_set_cols)?;
                        validate_foreign_key_set_columns(
                            &local_columns,
                            &on_delete_set_columns,
                            &cstr.fk_del_action,
                        )?;
                        foreign_keys.push(ForeignKey {
                            name: cname,
                            local_columns,
                            ref_table,
                            ref_columns,
                            on_update: compile_foreign_key_action(&cstr.fk_upd_action)?,
                            on_delete: compile_foreign_key_action(&cstr.fk_del_action)?,
                            on_delete_set_columns,
                            match_type: compile_foreign_key_match(&cstr.fk_matchtype)?,
                        });
                    }
                    pg_query::protobuf::ConstrType::ConstrPrimary
                    | pg_query::protobuf::ConstrType::ConstrUnique => {
                        let kind =
                            if cstr.contype() == pg_query::protobuf::ConstrType::ConstrPrimary {
                                if primary_key_seen {
                                    return Err(SQLError::TypeMismatch(
                                        "multiple PRIMARY KEY constraints are not allowed".into(),
                                    ));
                                }
                                primary_key_seen = true;
                                TableKeyConstraintKind::PrimaryKey
                            } else {
                                TableKeyConstraintKind::Unique
                            };
                        let key_columns = extract_strings(&cstr.keys)?;
                        key_constraints.push(TableKeyConstraint {
                            name: constraint_name(&cstr.conname),
                            kind,
                            columns: key_columns,
                            nulls_not_distinct: cstr.nulls_not_distinct,
                        });
                    }
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "table constraint {other:?} is not supported"
                        )));
                    }
                }
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE TABLE element {other:?} is not supported"
                )));
            }
        }
    }
    let column_names: BTreeSet<&str> = columns.iter().map(|column| column.name.as_str()).collect();
    for constraint in &key_constraints {
        if constraint.columns.is_empty() {
            return Err(SQLError::TypeMismatch(format!(
                "{} constraint must name at least one column",
                key_constraint_label(constraint.kind)
            )));
        }
        let mut seen = BTreeSet::new();
        for column in &constraint.columns {
            if !column_names.contains(column.as_str()) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint references unknown column `{column}`",
                    key_constraint_label(constraint.kind)
                )));
            }
            if !seen.insert(column.as_str()) {
                return Err(SQLError::TypeMismatch(format!(
                    "{} constraint names column `{column}` more than once",
                    key_constraint_label(constraint.kind)
                )));
            }
        }
    }
    // Keep legacy scalar-key consumers correct while retaining the full typed
    // tuple above. A composite primary key makes every member NOT NULL, but no
    // individual member is itself a primary/unique key.
    for constraint in &key_constraints {
        for column_name in &constraint.columns {
            let column = columns
                .iter_mut()
                .find(|column| column.name == *column_name)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "validated key column `{column_name}` disappeared during lowering"
                    ))
                })?;
            if constraint.kind == TableKeyConstraintKind::PrimaryKey {
                column.not_null = true;
                if constraint.columns.len() == 1 {
                    column.primary_key = true;
                }
            } else if constraint.columns.len() == 1 {
                column.unique = true;
            }
        }
    }
    Ok(CreateTable {
        name,
        columns,
        if_not_exists: stmt.if_not_exists,
        checks,
        foreign_keys,
        key_constraints,
    })
}

fn constraint_name(name: &str) -> Option<String> {
    (!name.is_empty()).then(|| name.to_string())
}

fn register_constraint_name(
    names: &mut std::collections::BTreeSet<String>,
    name: &str,
) -> Result<()> {
    if !name.is_empty() && !names.insert(name.to_string()) {
        return Err(SQLError::TypeMismatch(format!(
            "constraint `{name}` is declared more than once"
        )));
    }
    Ok(())
}

fn key_constraint_label(kind: TableKeyConstraintKind) -> &'static str {
    match kind {
        TableKeyConstraintKind::PrimaryKey => "PRIMARY KEY",
        TableKeyConstraintKind::Unique => "UNIQUE",
    }
}

pub(super) fn compile_column_def(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col)?;
    let ty = compile_type_name(col)?;
    let mut auto_increment = matches!(raw_type.as_deref(), Some("serial" | "bigserial"));
    let mut primary_key = false;
    let mut not_null = false;
    let mut unique = false;
    let mut default: Option<Expr> = None;
    let mut check: Option<Expr> = None;
    let mut references: Option<crate::ast::ForeignKeyRef> = None;
    for c in &col.constraints {
        let inner = c
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("column contains an empty constraint".into()))?;
        match inner {
            NodeEnum::Constraint(cstr) => match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => {
                    primary_key = true;
                    not_null = true;
                }
                pg_query::protobuf::ConstrType::ConstrNotnull => not_null = true,
                pg_query::protobuf::ConstrType::ConstrUnique => unique = true,
                pg_query::protobuf::ConstrType::ConstrIdentity => auto_increment = true,
                pg_query::protobuf::ConstrType::ConstrDefault => {
                    let raw = cstr.raw_expr.as_deref().ok_or_else(|| {
                        SQLError::Internal("DEFAULT constraint without expression".into())
                    })?;
                    default = Some(compile_expr(raw)?);
                }
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    let raw = cstr
                        .raw_expr
                        .as_deref()
                        .ok_or_else(|| SQLError::Internal("CHECK without expression".into()))?;
                    check = Some(compile_expr(raw)?);
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    let table =
                        cstr.pktable.as_ref().map(range_var_name).ok_or_else(|| {
                            SQLError::Internal("REFERENCES without a table".into())
                        })?;
                    let columns = extract_strings(&cstr.pk_attrs)?;
                    let [column] = columns.as_slice() else {
                        return Err(SQLError::Internal(
                            "column REFERENCES must name exactly one referenced column".into(),
                        ));
                    };
                    references = Some(crate::ast::ForeignKeyRef {
                        table,
                        column: column.clone(),
                        on_update: compile_foreign_key_action(&cstr.fk_upd_action)?,
                        on_delete: compile_foreign_key_action(&cstr.fk_del_action)?,
                        match_type: compile_foreign_key_match(&cstr.fk_matchtype)?,
                    });
                }
                pg_query::protobuf::ConstrType::ConstrNull => {}
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "column constraint {other:?} is not supported"
                    )));
                }
            },
            other => {
                return Err(SQLError::Internal(format!(
                    "unexpected column constraint node {other:?}"
                )));
            }
        }
    }
    // Postgres treats `SERIAL` / `BIGSERIAL` as `NOT NULL` by definition.
    if auto_increment {
        not_null = true;
    }
    Ok(ColumnDef {
        name,
        ty,
        primary_key,
        not_null,
        auto_increment,
        unique,
        default,
        check,
        references,
    })
}

// -------------------------------------------------------------------------
// CREATE INDEX
// -------------------------------------------------------------------------

pub(super) fn compile_create_index(stmt: &pg_query::protobuf::IndexStmt) -> Result<CreateIndex> {
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("CREATE INDEX without table".into()))?;
    let access_method = stmt.access_method.clone();
    let mut columns = Vec::new();
    for elt in &stmt.index_params {
        let inner = elt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE INDEX contains an empty key".into()))?;
        let NodeEnum::IndexElem(idx) = inner else {
            return Err(SQLError::Internal(format!(
                "CREATE INDEX expected IndexElem, got {inner:?}"
            )));
        };
        if idx.name.is_empty() {
            return Err(SQLError::Unsupported(
                "expression indexes are not supported".into(),
            ));
        }
        columns.push(idx.name.clone());
    }
    let name = if stmt.idxname.is_empty() {
        None
    } else {
        Some(stmt.idxname.clone())
    };
    let mut options = Vec::new();
    for opt in &stmt.options {
        let inner = opt
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CREATE INDEX contains an empty option".into()))?;
        let NodeEnum::DefElem(elem) = inner else {
            return Err(SQLError::Internal(format!(
                "CREATE INDEX expected DefElem option, got {inner:?}"
            )));
        };
        let key = elem.defname.clone();
        let value = match elem.arg.as_ref().and_then(|node| node.node.as_ref()) {
            Some(NodeEnum::String(value)) => value.sval.clone(),
            Some(NodeEnum::Integer(value)) => value.ival.to_string(),
            Some(NodeEnum::Float(value)) => value.fval.clone(),
            Some(NodeEnum::TypeName(value)) => extract_strings(&value.names)?.join("."),
            Some(other) => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE INDEX option `{key}` value {other:?}"
                )));
            }
            None => {
                return Err(SQLError::Internal(format!(
                    "CREATE INDEX option `{key}` has no value"
                )));
            }
        };
        options.push((key, value));
    }
    Ok(CreateIndex {
        name,
        table,
        access_method,
        columns,
        if_not_exists: stmt.if_not_exists,
        options,
    })
}

// -------------------------------------------------------------------------
// INSERT
// -------------------------------------------------------------------------

pub(super) fn compile_insert(stmt: &pg_query::protobuf::InsertStmt) -> Result<InsertStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("INSERT without relation".into()))?;
    let columns = stmt
        .cols
        .iter()
        .map(|column| match column.node.as_ref() {
            Some(NodeEnum::ResTarget(target)) if !target.name.is_empty() => Ok(target.name.clone()),
            other => Err(SQLError::Internal(format!(
                "INSERT column target is malformed: {other:?}"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    let select_node = stmt
        .select_stmt
        .as_ref()
        .ok_or_else(|| SQLError::Unsupported("INSERT without VALUES".into()))?;
    let select_inner = select_node
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("INSERT select_stmt empty".into()))?;
    let select = match select_inner {
        NodeEnum::SelectStmt(s) => s,
        _ => return Err(SQLError::Unsupported("INSERT body must be SELECT".into())),
    };
    let mut rows = Vec::new();
    for row_node in &select.values_lists {
        let inner = row_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("INSERT VALUES contains an empty row".into()))?;
        let list = match inner {
            NodeEnum::List(l) => l,
            other => {
                return Err(SQLError::Internal(format!(
                    "INSERT VALUES expected a row list, got {other:?}"
                )));
            }
        };
        let row: Vec<Expr> = list
            .items
            .iter()
            .map(compile_expr)
            .collect::<Result<Vec<_>>>()?;
        rows.push(row);
    }
    // INSERT ... SELECT: when the body has no values_lists but does
    // have a from_clause / target_list, treat it as `INSERT FROM
    // SELECT` and forward the inner SELECT.
    let select_source =
        if rows.is_empty() && (!select.from_clause.is_empty() || !select.target_list.is_empty()) {
            Some(Box::new(compile_select(select)?))
        } else {
            None
        };
    let on_conflict = stmt
        .on_conflict_clause
        .as_ref()
        .map(|c| compile_on_conflict(c.as_ref()))
        .transpose()?;
    let returning = compile_projections(&stmt.returning_list)?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(InsertStmt {
        table,
        columns,
        with,
        rows,
        select_source,
        on_conflict,
        returning,
    })
}

fn compile_on_conflict(
    clause: &pg_query::protobuf::OnConflictClause,
) -> Result<crate::ast::OnConflict> {
    use crate::ast::{OnConflict, OnConflictAction};
    use pg_query::protobuf::OnConflictAction as PgAction;

    let conflict_columns = clause
        .infer
        .as_ref()
        .map(|infer| {
            infer
                .index_elems
                .iter()
                .map(|elem| match elem.node.as_ref() {
                    Some(NodeEnum::IndexElem(index)) if !index.name.is_empty() => {
                        Ok(index.name.clone())
                    }
                    other => Err(SQLError::Unsupported(format!(
                        "ON CONFLICT inference target {other:?}"
                    ))),
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    let action = match clause.action() {
        PgAction::OnconflictNothing => OnConflictAction::Nothing,
        PgAction::OnconflictUpdate => {
            let mut assignments: Vec<(String, Expr)> = Vec::new();
            for tgt in &clause.target_list {
                let inner = tgt.node.as_ref().ok_or_else(|| {
                    SQLError::Internal("ON CONFLICT UPDATE contains an empty assignment".into())
                })?;
                let NodeEnum::ResTarget(rt) = inner else {
                    return Err(SQLError::Internal(format!(
                        "ON CONFLICT UPDATE expected ResTarget, got {inner:?}"
                    )));
                };
                let val = rt.val.as_ref().ok_or_else(|| {
                    SQLError::Internal("ON CONFLICT UPDATE assignment has no value".into())
                })?;
                let expr = compile_expr(val)?;
                assignments.push((rt.name.clone(), expr));
            }
            let where_clause = clause
                .where_clause
                .as_ref()
                .map(|w| compile_expr(w))
                .transpose()?;
            OnConflictAction::Update {
                assignments,
                r#where: where_clause,
            }
        }
        PgAction::OnconflictNone | PgAction::Undefined => {
            return Err(SQLError::Unsupported(
                "ON CONFLICT without action specifier".into(),
            ));
        }
    };

    Ok(OnConflict {
        conflict_columns,
        action,
    })
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

pub(super) fn compile_from_node(node: &Node) -> Result<FromClause> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty FROM node".into()));
    };
    match inner {
        NodeEnum::RangeVar(r) => {
            if !r.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "cross-database relation references are not supported".into(),
                ));
            }
            Ok(FromClause::Table {
                name: range_var_name(r),
                alias: r.alias.as_ref().and_then(|a| {
                    if a.aliasname.is_empty() {
                        None
                    } else {
                        Some(a.aliasname.clone())
                    }
                }),
            })
        }
        NodeEnum::JoinExpr(j) => {
            if j.is_natural {
                return Err(SQLError::Unsupported(
                    "NATURAL JOIN is not supported".into(),
                ));
            }
            if !j.using_clause.is_empty() || j.join_using_alias.is_some() {
                return Err(SQLError::Unsupported("JOIN USING is not supported".into()));
            }
            if j.alias.is_some() {
                return Err(SQLError::Unsupported(
                    "aliases on parenthesized JOIN expressions are not supported".into(),
                ));
            }
            let left = j
                .larg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing left".into()))?;
            let right = j
                .rarg
                .as_ref()
                .ok_or_else(|| SQLError::Internal("JOIN missing right".into()))?;
            let kind = match j.jointype() {
                pg_query::protobuf::JoinType::JoinInner => JoinKind::Inner,
                pg_query::protobuf::JoinType::JoinLeft => JoinKind::Left,
                pg_query::protobuf::JoinType::JoinRight => JoinKind::Right,
                pg_query::protobuf::JoinType::JoinFull => JoinKind::Full,
                other => {
                    return Err(SQLError::Unsupported(format!("JOIN type {other:?}")));
                }
            };
            let on = j.quals.as_deref().map(compile_expr).transpose()?;
            let lateral = right_is_lateral(right);
            Ok(FromClause::Join {
                left: Box::new(compile_from_node(left)?),
                right: Box::new(compile_from_node(right)?),
                kind,
                on,
                lateral,
            })
        }
        NodeEnum::RangeSubselect(rs) => {
            let body_node = rs
                .subquery
                .as_deref()
                .ok_or_else(|| SQLError::Internal("FROM (subquery) without body".into()))?;
            let inner = body_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("subquery body empty".into()))?;
            let select = match inner {
                NodeEnum::SelectStmt(s) => compile_select(s)?,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "FROM (subquery) body must be SELECT, got {other:?}"
                    )));
                }
            };
            // Standalone VALUES land here as a SelectStmt with empty
            // target_list and a values_lists -- promote to
            // FromClause::Values for the engine fast path.
            let (alias, column_aliases) = compile_alias(rs.alias.as_ref())?;
            let body_inner = body_node
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("subquery body empty".into()))?;
            if let NodeEnum::SelectStmt(s) = body_inner {
                if !s.values_lists.is_empty() {
                    let mut rows: Vec<Vec<Expr>> = Vec::new();
                    for r in &s.values_lists {
                        let Some(NodeEnum::List(list)) = r.node.as_ref() else {
                            return Err(SQLError::Internal(
                                "VALUES subquery contains a malformed row".into(),
                            ));
                        };
                        let row: Vec<Expr> = list
                            .items
                            .iter()
                            .map(compile_expr)
                            .collect::<Result<Vec<_>>>()?;
                        rows.push(row);
                    }
                    return Ok(FromClause::Values {
                        rows,
                        alias,
                        column_aliases,
                    });
                }
            }
            Ok(FromClause::Subquery {
                body: Box::new(select),
                alias,
                column_aliases,
            })
        }
        NodeEnum::RangeFunction(rf) => {
            if rf.is_rowsfrom || rf.functions.len() > 1 {
                return Err(SQLError::Unsupported(
                    "ROWS FROM and multiple table functions in one FROM item are not supported"
                        .into(),
                ));
            }
            if rf.ordinality {
                return Err(SQLError::Unsupported(
                    "table functions WITH ORDINALITY are not supported".into(),
                ));
            }
            // A single function in `functions` carries the call. Re-use
            // compile_expr to lift it into an Expr::Func, then peel back
            // the name and arguments.
            let Some(first_node) = rf.functions.first() else {
                return Err(SQLError::Internal("RangeFunction without functions".into()));
            };
            // RangeFunction.functions is a list of `[FuncCall, alias_definition]`
            // pairs encoded as a List. Take the first element of the
            // first pair as the call.
            let call = match first_node.node.as_ref() {
                Some(NodeEnum::List(l)) => l
                    .items
                    .first()
                    .ok_or_else(|| SQLError::Internal("RangeFunction empty pair".into()))?,
                _ => first_node,
            };
            let expr = compile_expr(call)?;
            let (name, args) = match expr {
                Expr::Func { name, args, .. } => (name, args),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "RangeFunction body must be a function call, got {other:?}"
                    )));
                }
            };
            let (alias, column_aliases) = compile_alias(rf.alias.as_ref())?;
            let coldefs = compile_column_definitions(&rf.coldeflist)?;
            let column_types: Vec<String> = coldefs.iter().map(|(_, ty)| ty.clone()).collect();
            let coldef_aliases: Vec<String> = coldefs.into_iter().map(|(name, _)| name).collect();
            Ok(FromClause::Function {
                name,
                args,
                alias,
                column_aliases: if coldef_aliases.is_empty() {
                    column_aliases
                } else {
                    coldef_aliases
                },
                column_types,
            })
        }
        other => Err(SQLError::Unsupported(format!("FROM form: {other:?}"))),
    }
}

fn right_is_lateral(node: &Node) -> bool {
    match node.node.as_ref() {
        Some(NodeEnum::RangeSubselect(rs)) => rs.lateral,
        Some(NodeEnum::RangeFunction(rf)) => rf.lateral,
        _ => false,
    }
}

fn compile_alias(
    alias: Option<&pg_query::protobuf::Alias>,
) -> Result<(Option<String>, Vec<String>)> {
    let Some(a) = alias else {
        return Ok((None, Vec::new()));
    };
    let name = if a.aliasname.is_empty() {
        None
    } else {
        Some(a.aliasname.clone())
    };
    let cols = extract_strings(&a.colnames)?;
    Ok((name, cols))
}

/// Column definition list entries as `(name, lowercased type name)`.
/// The type name is the last component of the `TypeName` path
/// (`pg_catalog.int4` -> `int4`, `agtype` -> `agtype`); empty when the
/// definition omitted a type.
fn compile_column_definitions(nodes: &[Node]) -> Result<Vec<(String, String)>> {
    nodes
        .iter()
        .map(|node| match node.node.as_ref() {
            Some(NodeEnum::ColumnDef(col)) => {
                let type_node = col.type_name.as_ref().ok_or_else(|| {
                    SQLError::Internal(format!("function column `{}` has no type", col.colname))
                })?;
                let type_name = extract_strings(&type_node.names)?
                    .last()
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "function column `{}` has an empty type name",
                            col.colname
                        ))
                    })?
                    .to_ascii_lowercase();
                Ok((col.colname.clone(), type_name))
            }
            other => Err(SQLError::Unsupported(format!(
                "function column definition: {other:?}"
            ))),
        })
        .collect()
}

pub(super) fn compile_select(stmt: &pg_query::protobuf::SelectStmt) -> Result<SelectStmt> {
    if stmt.into_clause.is_some() {
        return Err(SQLError::Unsupported("SELECT INTO is not supported".into()));
    }
    if stmt.group_distinct {
        return Err(SQLError::Unsupported(
            "GROUP BY DISTINCT is not supported".into(),
        ));
    }
    if !stmt.window_clause.is_empty() {
        return Err(SQLError::Unsupported(
            "named WINDOW clauses are not supported".into(),
        ));
    }
    if stmt.limit_option() == pg_query::protobuf::LimitOption::WithTies {
        return Err(SQLError::Unsupported(
            "FETCH ... WITH TIES is not supported".into(),
        ));
    }
    if !stmt.locking_clause.is_empty() {
        return Err(SQLError::Unsupported(
            "SELECT row-locking clauses are not supported".into(),
        ));
    }
    let from = compile_from_list(&stmt.from_clause)?;
    let projections = compile_projections(&stmt.target_list)?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let order_by = compile_order_by(&stmt.sort_clause)?;
    let limit = compile_limit_offset_expr(stmt.limit_count.as_deref())?;
    let offset = compile_limit_offset_expr(stmt.limit_offset.as_deref())?;
    let (group_by, grouping_sets) = compile_group_clause(&stmt.group_clause)?;
    // Resolve GROUP BY 1 / GROUP BY <alias> against the SELECT list.
    // Postgres prefers a real column when one matches, falling back to
    // the alias; we don't have schema info here, so we only rewrite
    // when the alias clearly cannot be a column on the source row
    // (i.e., the projection's expression is something other than a
    // bare reference to that same name).
    let group_by = resolve_group_by_aliases(group_by, &projections);
    let grouping_sets: Vec<Vec<Expr>> = grouping_sets
        .into_iter()
        .map(|s| resolve_group_by_aliases(s, &projections))
        .collect();
    let having = stmt
        .having_clause
        .as_ref()
        .map(|h| compile_expr(h))
        .transpose()?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    let mut set_op = compile_set_op(stmt)?;

    // For UNION / INTERSECT / EXCEPT shapes the outer SelectStmt carries:
    //   * its own `sortClause` / `limitCount` / `limitOffset` -> the
    //     *combined* ORDER BY / LIMIT / OFFSET applied to `lhs <op> rhs`
    //     (those land on `set_op.combined_*`).
    //   * empty `targetList` / `fromClause`; the LHS branch (with its
    //     own clauses, including its own optional `ORDER BY` / `LIMIT`)
    //     lives in `stmt.larg`. We preserve that full subtree on `SetOp::left`
    //     and mirror its basic clauses on the parent for output-column
    //     discovery and backward compatibility with serialized AST users.
    let (projections, from, r#where, group_by, order_by, limit, offset) = if set_op.is_some() {
        // Promote the outer (combined) clauses onto the SetOp and
        // replace the parent's clauses with the LHS branch's.
        if let Some(so) = set_op.as_mut() {
            so.combined_order_by = order_by;
            so.combined_limit = limit;
            so.combined_offset = offset;
        }
        let lhs_node = stmt
            .larg
            .as_deref()
            .ok_or_else(|| SQLError::Internal("set op missing left".into()))?;
        let lhs = compile_select(lhs_node)?;
        if let Some(so) = set_op.as_mut() {
            so.left = Some(Box::new(lhs.clone()));
        }
        (
            lhs.projections,
            lhs.from,
            lhs.r#where,
            lhs.group_by,
            lhs.order_by,
            lhs.limit,
            lhs.offset,
        )
    } else {
        (
            projections,
            from,
            r#where,
            group_by,
            order_by,
            limit,
            offset,
        )
    };

    let (distinct, distinct_on) = compile_distinct_clause(&stmt.distinct_clause)?;

    Ok(SelectStmt {
        projections,
        from,
        r#where,
        group_by,
        grouping_sets,
        having,
        order_by,
        limit,
        offset,
        with,
        set_op,
        distinct,
        distinct_on,
    })
}

fn compile_distinct_clause(nodes: &[Node]) -> Result<(bool, Vec<Expr>)> {
    if nodes.is_empty() {
        return Ok((false, Vec::new()));
    }
    let mut distinct_on = Vec::new();
    for node in nodes {
        match node.node.as_ref() {
            None => return Ok((true, Vec::new())),
            Some(NodeEnum::AConst(c)) if c.isnull || c.val.is_none() => {
                return Ok((true, Vec::new()));
            }
            Some(_) => distinct_on.push(compile_expr(node)?),
        }
    }
    Ok((true, distinct_on))
}

fn compile_from_list(nodes: &[Node]) -> Result<Option<FromClause>> {
    let Some(first) = nodes.first() else {
        return Ok(None);
    };
    let mut current = compile_from_node(first)?;
    for node in nodes.iter().skip(1) {
        let lateral = right_is_lateral(node);
        current = FromClause::Join {
            left: Box::new(current),
            right: Box::new(compile_from_node(node)?),
            kind: JoinKind::Cross,
            on: None,
            lateral,
        };
    }
    Ok(Some(current))
}

fn resolve_group_by_aliases(group_by: Vec<Expr>, projections: &[Projection]) -> Vec<Expr> {
    group_by
        .into_iter()
        .map(|g| match &g {
            // GROUP BY <ordinal>: refers to the Nth projection.
            Expr::Literal(Value::Int(n)) if *n >= 1 => match usize::try_from(*n) {
                Ok(position) if position <= projections.len() => {
                    projections[position - 1].expr.clone()
                }
                _ => g,
            },
            // GROUP BY <alias>: only rewrite when the alias points at
            // a non-trivial expression. If the projection is just a
            // column reference with the same name the original AST is
            // already correct.
            Expr::Column(name) => {
                for p in projections {
                    if let Some(alias) = &p.alias {
                        if alias == name {
                            if let Expr::Column(col_name) = &p.expr {
                                if col_name == name {
                                    return g;
                                }
                            }
                            return p.expr.clone();
                        }
                    }
                }
                g
            }
            _ => g,
        })
        .collect()
}

fn compile_group_clause(nodes: &[pg_query::protobuf::Node]) -> Result<(Vec<Expr>, Vec<Vec<Expr>>)> {
    use pg_query::protobuf::GroupingSetKind;

    fn simple_item(node: &pg_query::protobuf::Node) -> Result<Vec<Expr>> {
        match node.node.as_ref() {
            Some(NodeEnum::GroupingSet(grouping)) => match grouping.kind() {
                GroupingSetKind::GroupingSetEmpty => Ok(Vec::new()),
                GroupingSetKind::GroupingSetSimple => grouping
                    .content
                    .iter()
                    .map(compile_expr)
                    .collect::<Result<Vec<_>>>(),
                other => Err(SQLError::Unsupported(format!(
                    "nested grouping item {other:?} is not a simple grouping key"
                ))),
            },
            Some(_) => Ok(vec![compile_expr(node)?]),
            None => Err(SQLError::Internal(
                "GROUP BY contains an empty parse node".into(),
            )),
        }
    }

    fn expand(grouping: &pg_query::protobuf::GroupingSet) -> Result<Vec<Vec<Expr>>> {
        match grouping.kind() {
            GroupingSetKind::GroupingSetEmpty => Ok(vec![Vec::new()]),
            GroupingSetKind::GroupingSetSimple => Ok(vec![grouping
                .content
                .iter()
                .map(simple_item)
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()]),
            GroupingSetKind::GroupingSetRollup => {
                let items = grouping
                    .content
                    .iter()
                    .map(simple_item)
                    .collect::<Result<Vec<_>>>()?;
                let set_count = items.len().checked_add(1).ok_or_else(|| {
                    SQLError::Unsupported("ROLLUP has too many grouping items".into())
                })?;
                let mut sets = Vec::new();
                sets.try_reserve(set_count).map_err(|error| {
                    SQLError::Unsupported(format!(
                        "ROLLUP expansion of {set_count} grouping sets is too large: {error}"
                    ))
                })?;
                for prefix in (0..=items.len()).rev() {
                    sets.push(items[..prefix].iter().flatten().cloned().collect());
                }
                Ok(sets)
            }
            GroupingSetKind::GroupingSetCube => {
                let items = grouping
                    .content
                    .iter()
                    .map(simple_item)
                    .collect::<Result<Vec<_>>>()?;
                let shift = u32::try_from(items.len()).map_err(|_| {
                    SQLError::Unsupported(format!(
                        "CUBE has too many grouping items: {}",
                        items.len()
                    ))
                })?;
                let set_count = 1_usize.checked_shl(shift).ok_or_else(|| {
                    SQLError::Unsupported(format!(
                        "CUBE has too many grouping items: {}",
                        items.len()
                    ))
                })?;
                let mut sets = Vec::new();
                sets.try_reserve(set_count).map_err(|error| {
                    SQLError::Unsupported(format!(
                        "CUBE expansion of {set_count} grouping sets is too large: {error}"
                    ))
                })?;
                for mask in 0..set_count {
                    let mut set = Vec::new();
                    for (index, item) in items.iter().enumerate() {
                        if mask & (1_usize << index) != 0 {
                            set.extend(item.iter().cloned());
                        }
                    }
                    sets.push(set);
                }
                Ok(sets)
            }
            GroupingSetKind::GroupingSetSets => {
                let mut sets = Vec::new();
                for child in &grouping.content {
                    match child.node.as_ref() {
                        Some(NodeEnum::GroupingSet(nested)) => sets.extend(expand(nested)?),
                        Some(_) => sets.push(vec![compile_expr(child)?]),
                        None => {
                            return Err(SQLError::Internal(
                                "GROUPING SETS contains an empty parse node".into(),
                            ))
                        }
                    }
                }
                Ok(sets)
            }
            other => Err(SQLError::Unsupported(format!(
                "GROUP BY grouping-set kind {other:?}"
            ))),
        }
    }

    let mut plain = Vec::new();
    let mut combined_sets = vec![Vec::new()];
    let mut has_grouping_set = false;
    for node in nodes {
        let alternatives = match node.node.as_ref() {
            Some(NodeEnum::GroupingSet(grouping)) => {
                has_grouping_set = true;
                expand(grouping)?
            }
            Some(_) => {
                let expression = compile_expr(node)?;
                plain.push(expression.clone());
                vec![vec![expression]]
            }
            None => {
                return Err(SQLError::Internal(
                    "GROUP BY contains an empty parse node".into(),
                ))
            }
        };

        let mut product = Vec::new();
        let product_count = combined_sets
            .len()
            .checked_mul(alternatives.len())
            .ok_or_else(|| {
                SQLError::Unsupported("GROUP BY expansion count overflowed usize".into())
            })?;
        product.try_reserve(product_count).map_err(|error| {
            SQLError::Unsupported(format!("GROUP BY expansion is too large: {error}"))
        })?;
        for prefix in &combined_sets {
            for alternative in &alternatives {
                let mut set = prefix.clone();
                set.extend(alternative.iter().cloned());
                product.push(set);
            }
        }
        combined_sets = product;
    }

    if has_grouping_set {
        Ok((Vec::new(), combined_sets))
    } else {
        Ok((plain, Vec::new()))
    }
}

pub(super) fn compile_projections(targets: &[pg_query::protobuf::Node]) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(targets.len());
    for target_node in targets {
        let inner = target_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SELECT contains an empty target".into()))?;
        let res_target = match inner {
            NodeEnum::ResTarget(t) => t,
            _ => return Err(SQLError::Internal(format!("unexpected target {inner:?}"))),
        };
        let alias = if res_target.name.is_empty() {
            None
        } else {
            Some(res_target.name.clone())
        };
        let expr = match &res_target.val {
            Some(node) => compile_expr(node)?,
            None => return Err(SQLError::Internal("ResTarget without value".into())),
        };
        out.push(Projection { expr, alias });
    }
    Ok(out)
}

fn compile_order_by(sort_clause: &[pg_query::protobuf::Node]) -> Result<Vec<OrderBy>> {
    let mut out = Vec::with_capacity(sort_clause.len());
    for sort_node in sort_clause {
        let inner = sort_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ORDER BY contains an empty item".into()))?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
        let expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "ORDER BY")?;
        out.push(OrderBy {
            expr,
            descending,
            nulls,
        });
    }
    Ok(out)
}

fn compile_sort_options(
    sort: &pg_query::protobuf::SortBy,
    context: &str,
) -> Result<(bool, Option<crate::ast::NullsOrder>)> {
    use pg_query::protobuf::{SortByDir, SortByNulls};

    let direction = SortByDir::try_from(sort.sortby_dir).map_err(|_| {
        SQLError::Internal(format!(
            "{context} has invalid sort direction {}",
            sort.sortby_dir
        ))
    })?;
    let descending = match direction {
        SortByDir::SortbyDefault | SortByDir::SortbyAsc => false,
        SortByDir::SortbyDesc => true,
        SortByDir::SortbyUsing => {
            return Err(SQLError::Unsupported(format!(
                "{context} USING operators are not represented by OrderBy"
            )));
        }
        SortByDir::Undefined => {
            return Err(SQLError::Internal(format!(
                "{context} has an undefined sort direction"
            )));
        }
    };
    let null_order = SortByNulls::try_from(sort.sortby_nulls).map_err(|_| {
        SQLError::Internal(format!(
            "{context} has invalid NULLS ordering {}",
            sort.sortby_nulls
        ))
    })?;
    let nulls = match null_order {
        SortByNulls::SortbyNullsDefault => None,
        SortByNulls::SortbyNullsFirst => Some(crate::ast::NullsOrder::First),
        SortByNulls::SortbyNullsLast => Some(crate::ast::NullsOrder::Last),
        SortByNulls::Undefined => {
            return Err(SQLError::Internal(format!(
                "{context} has an undefined NULLS ordering"
            )));
        }
    };
    Ok((descending, nulls))
}

fn compile_set_op(stmt: &pg_query::protobuf::SelectStmt) -> Result<Option<Box<SetOp>>> {
    let kind = match stmt.op() {
        pg_query::protobuf::SetOperation::SetopNone => return Ok(None),
        pg_query::protobuf::SetOperation::SetopUnion => SetOpKind::Union,
        pg_query::protobuf::SetOperation::SetopIntersect => SetOpKind::Intersect,
        pg_query::protobuf::SetOperation::SetopExcept => SetOpKind::Except,
        other => return Err(SQLError::Unsupported(format!("set op {other:?}"))),
    };
    if stmt.larg.is_none() {
        return Err(SQLError::Internal("set op missing left".into()));
    }
    let right_node = stmt
        .rarg
        .as_deref()
        .ok_or_else(|| SQLError::Internal("set op missing right".into()))?;
    let right = compile_select(right_node)?;
    Ok(Some(Box::new(SetOp {
        kind,
        all: stmt.all,
        left: None,
        right,
        // The outer SelectStmt's ORDER BY / LIMIT / OFFSET land here
        // when `compile_select` finishes - the caller fills these in
        // because at this point we don't have the parent's clauses
        // resolved yet. Default to empty / None until then.
        combined_order_by: Vec::new(),
        combined_limit: None,
        combined_offset: None,
    })))
}

pub(super) fn compile_with_clause(wc: &pg_query::protobuf::WithClause) -> Result<Vec<CTE>> {
    let mut out = Vec::with_capacity(wc.ctes.len());
    for cte_node in &wc.ctes {
        let inner = cte_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("WITH contains an empty CTE".into()))?;
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SQLError::Internal("expected CommonTableExpr".into())),
        };
        if cte.ctename.is_empty() {
            return Err(SQLError::Internal("CTE name is empty".into()));
        }
        if cte.search_clause.is_some() {
            return Err(SQLError::Unsupported(
                "recursive CTE SEARCH clauses are not supported".into(),
            ));
        }
        if cte.cycle_clause.is_some() {
            return Err(SQLError::Unsupported(
                "recursive CTE CYCLE clauses are not supported".into(),
            ));
        }
        match cte.ctematerialized() {
            pg_query::protobuf::CteMaterialize::CtematerializeUndefined
            | pg_query::protobuf::CteMaterialize::Default
            | pg_query::protobuf::CteMaterialize::Always => {}
            pg_query::protobuf::CteMaterialize::Never => {
                return Err(SQLError::Unsupported(
                    "CTE NOT MATERIALIZED is not supported".into(),
                ));
            }
        }
        let select_node = cte
            .ctequery
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE without query".into()))?;
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CTE query node empty".into()))?;
        let select = match select_inner {
            NodeEnum::SelectStmt(s) => s,
            _ => return Err(SQLError::Unsupported("CTE body must be SELECT".into())),
        };
        let columns = extract_strings(&cte.aliascolnames)?;
        out.push(CTE {
            name: cte.ctename.clone(),
            columns,
            recursive: wc.recursive,
            query: Box::new(compile_select(select)?),
        });
    }
    Ok(out)
}

/// Compile a `LIMIT` / `OFFSET` operand into an [`Expr`]. The
/// expression is resolved to an integer at execute time, so `LIMIT $1`
/// and other parameter-bearing forms work end-to-end. `None` means the
/// clause was absent entirely (`SELECT ... LIMIT NULL` is also `None`
/// because PG treats `NULL` as "no limit").
fn compile_limit_offset_expr(node: Option<&Node>) -> Result<Option<Expr>> {
    use pg_query::protobuf::a_const::Val;
    let Some(node) = node else { return Ok(None) };
    let inner = node
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("LIMIT/OFFSET contains an empty expression".into()))?;
    // `SELECT ... LIMIT NULL` parses as an `AConst` with no `val` --
    // treat it like an absent clause.
    if let NodeEnum::AConst(c) = inner {
        if c.val.is_none() {
            return Ok(None);
        }
        if let Some(Val::Ival(i)) = &c.val {
            if i.ival < 0 {
                return Err(SQLError::Internal("negative LIMIT/OFFSET".into()));
            }
        }
    }
    Ok(Some(compile_expr(node)?))
}

// -------------------------------------------------------------------------
// Expression compiler
// -------------------------------------------------------------------------

pub(super) fn compile_expr(node: &Node) -> Result<Expr> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("missing expr node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => compile_const(c),
        NodeEnum::ColumnRef(c) => compile_column_ref(c),
        NodeEnum::ParamRef(p) => {
            let index = usize::try_from(p.number).map_err(|_| {
                SQLError::Internal(format!(
                    "parameter index must be positive, got {}",
                    p.number
                ))
            })?;
            if index == 0 {
                return Err(SQLError::Internal(
                    "parameter index must be greater than zero".into(),
                ));
            }
            Ok(Expr::Param(index))
        }
        NodeEnum::FuncCall(f) => compile_func_call(f),
        NodeEnum::NamedArgExpr(arg) => {
            if arg.name.is_empty() {
                return Err(SQLError::Internal(
                    "NamedArgExpr without an argument name".into(),
                ));
            }
            let Some(value_node) = arg.arg.as_ref() else {
                return Err(SQLError::Internal("NamedArgExpr without value".into()));
            };
            Ok(Expr::Func {
                name: "__named_arg".into(),
                args: vec![
                    Expr::Literal(Value::Str(arg.name.clone())),
                    compile_expr(value_node)?,
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::AArrayExpr(a) => {
            let elements: Vec<Expr> = a
                .elements
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Array(elements))
        }
        NodeEnum::TypeCast(tc) => compile_type_cast(tc),
        NodeEnum::AExpr(a) => compile_a_expr(a),
        NodeEnum::SqlvalueFunction(svf) => compile_sql_value_function(svf),
        NodeEnum::MergeSupportFunc(_) => Ok(Expr::Func {
            name: "merge_action".into(),
            args: Vec::new(),
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        }),
        NodeEnum::BoolExpr(b) => compile_bool_expr(b),
        NodeEnum::NullTest(n) => compile_null_test(n),
        NodeEnum::CaseExpr(c) => compile_case_expr(c),
        NodeEnum::CoalesceExpr(ce) => {
            if ce.args.is_empty() {
                return Err(SQLError::Internal("COALESCE without arguments".into()));
            }
            let args: Vec<Expr> = ce
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Func {
                name: "coalesce".into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::MinMaxExpr(me) => {
            use pg_query::protobuf::MinMaxOp;
            let name = match me.op() {
                MinMaxOp::IsGreatest => "greatest",
                MinMaxOp::IsLeast => "least",
                _ => {
                    return Err(SQLError::Unsupported(format!(
                        "MinMaxExpr op {:?}",
                        me.op()
                    )));
                }
            };
            let args: Vec<Expr> = me
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            if args.is_empty() {
                return Err(SQLError::Internal(format!(
                    "{} without arguments",
                    name.to_ascii_uppercase()
                )));
            }
            Ok(Expr::Func {
                name: name.into(),
                args,
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        NodeEnum::SubLink(sl) => compile_sublink(sl),
        // ROW(a, b) constructors compare element-wise; the evaluator
        // reuses the list comparison rules for them.
        NodeEnum::RowExpr(row) => {
            let elements: Vec<Expr> = row
                .args
                .iter()
                .map(compile_expr)
                .collect::<Result<Vec<_>>>()?;
            Ok(Expr::Array(elements))
        }
        NodeEnum::AIndirection(ind) => compile_indirection(ind),
        other => Err(SQLError::Unsupported(format!("expression form: {other:?}"))),
    }
}

/// `expr[i]`, `expr[lo:hi]`, and chains thereof. Subscripts are
/// 1-based; slices clamp to the array, both per `PostgreSQL`.
fn compile_indirection(ind: &pg_query::protobuf::AIndirection) -> Result<Expr> {
    let base = ind
        .arg
        .as_deref()
        .ok_or_else(|| SQLError::Internal("AIndirection without base".into()))?;
    let mut current = compile_expr(base)?;
    if ind.indirection.is_empty() {
        return Err(SQLError::Internal(
            "AIndirection without indirection steps".into(),
        ));
    }
    for step in &ind.indirection {
        let inner = step
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("indirection contains an empty step".into()))?;
        match inner {
            NodeEnum::AIndices(idx) => {
                if idx.is_slice {
                    let lower = idx
                        .lidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .unwrap_or(Expr::Literal(Value::Null));
                    let upper = idx
                        .uidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .unwrap_or(Expr::Literal(Value::Null));
                    current = Expr::Func {
                        name: "__slice".into(),
                        args: vec![current, lower, upper],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                } else {
                    let index = idx
                        .uidx
                        .as_deref()
                        .map(compile_expr)
                        .transpose()?
                        .ok_or_else(|| SQLError::Internal("subscript without index".into()))?;
                    current = Expr::Func {
                        name: "__subscript".into(),
                        args: vec![current, index],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                }
            }
            NodeEnum::String(field) => {
                if field.sval.is_empty() {
                    return Err(SQLError::Internal(
                        "indirection contains an empty field name".into(),
                    ));
                }
                // `(composite).field` access on map values.
                current = Expr::Func {
                    name: "__subscript".into(),
                    args: vec![current, Expr::Literal(Value::Str(field.sval.clone()))],
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                };
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "indirection step: {other:?}"
                )));
            }
        }
    }
    Ok(current)
}

fn compile_sql_value_function(svf: &pg_query::protobuf::SqlValueFunction) -> Result<Expr> {
    use pg_query::protobuf::SqlValueFunctionOp;
    let name = match svf.op() {
        SqlValueFunctionOp::SvfopCurrentDate => "current_date",
        SqlValueFunctionOp::SvfopCurrentTimestamp
        | SqlValueFunctionOp::SvfopCurrentTimestampN
        | SqlValueFunctionOp::SvfopLocaltimestamp
        | SqlValueFunctionOp::SvfopLocaltimestampN
        | SqlValueFunctionOp::SvfopCurrentTime
        | SqlValueFunctionOp::SvfopCurrentTimeN
        | SqlValueFunctionOp::SvfopLocaltime
        | SqlValueFunctionOp::SvfopLocaltimeN => "current_timestamp",
        SqlValueFunctionOp::SvfopCurrentSchema => "current_schema",
        SqlValueFunctionOp::SvfopCurrentCatalog => "current_database",
        SqlValueFunctionOp::SvfopCurrentUser
        | SqlValueFunctionOp::SvfopCurrentRole
        | SqlValueFunctionOp::SvfopSessionUser
        | SqlValueFunctionOp::SvfopUser => "current_user",
        other => {
            return Err(SQLError::Unsupported(format!(
                "SQL value function {other:?}"
            )));
        }
    };
    Ok(Expr::Func {
        name: name.into(),
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    })
}

fn compile_sublink(sl: &pg_query::protobuf::SubLink) -> Result<Expr> {
    use pg_query::protobuf::SubLinkType;
    let body_node = sl
        .subselect
        .as_deref()
        .ok_or_else(|| SQLError::Internal("SubLink without subselect".into()))?;
    let inner_select = match body_node.node.as_ref() {
        Some(NodeEnum::SelectStmt(s)) => compile_select(s)?,
        _ => {
            return Err(SQLError::Unsupported("SubLink body must be SELECT".into()));
        }
    };
    let body = Box::new(inner_select);
    let operator = if sl.oper_name.is_empty() {
        None
    } else {
        Some(extract_strings(&sl.oper_name)?.join(""))
    };
    match sl.sub_link_type() {
        SubLinkType::ExprSublink => {
            if sl.testexpr.is_some() || operator.is_some() {
                return Err(SQLError::Internal(
                    "scalar SubLink unexpectedly has a test expression or operator".into(),
                ));
            }
            Ok(Expr::ScalarSubquery(body))
        }
        SubLinkType::ExistsSublink => {
            if sl.testexpr.is_some() || operator.is_some() {
                return Err(SQLError::Internal(
                    "EXISTS SubLink unexpectedly has a test expression or operator".into(),
                ));
            }
            Ok(Expr::Exists {
                body,
                negated: false,
            })
        }
        SubLinkType::AnySublink => {
            if !matches!(operator.as_deref(), None | Some("=")) {
                return Err(SQLError::Unsupported(format!(
                    "ANY subquery operator `{}` is not represented by InSubquery",
                    operator.as_deref().unwrap_or("")
                )));
            }
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ANY SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: false,
            })
        }
        SubLinkType::AllSublink => {
            if operator.as_deref() != Some("<>") {
                return Err(SQLError::Unsupported(format!(
                    "ALL subquery operator `{}` is not represented by InSubquery",
                    operator.as_deref().unwrap_or("")
                )));
            }
            // `lhs <> ALL (subquery)` is SQL's `lhs NOT IN (subquery)`.
            let testexpr = sl
                .testexpr
                .as_deref()
                .ok_or_else(|| SQLError::Internal("ALL SubLink without testexpr".into()))?;
            Ok(Expr::InSubquery {
                expr: Box::new(compile_expr(testexpr)?),
                body,
                negated: true,
            })
        }
        other => Err(SQLError::Unsupported(format!("SubLink type {other:?}"))),
    }
}

fn compile_case_expr(c: &pg_query::protobuf::CaseExpr) -> Result<Expr> {
    let base = c
        .arg
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    let mut when: Vec<(Expr, Expr)> = Vec::with_capacity(c.args.len());
    if c.args.is_empty() {
        return Err(SQLError::Internal(
            "CASE expression without WHEN arms".into(),
        ));
    }
    for arm in &c.args {
        let inner = arm
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE arm without body".into()))?;
        let NodeEnum::CaseWhen(cw) = inner else {
            return Err(SQLError::Internal(format!(
                "CASE arm expected CaseWhen, got {inner:?}"
            )));
        };
        let cond = cw
            .expr
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without cond".into()))?;
        let result = cw
            .result
            .as_ref()
            .ok_or_else(|| SQLError::Internal("CASE WHEN without THEN".into()))?;
        when.push((compile_expr(cond)?, compile_expr(result)?));
    }
    let else_branch = c
        .defresult
        .as_ref()
        .map(|n| compile_expr(n))
        .transpose()?
        .map(Box::new);
    Ok(Expr::Case {
        base,
        when,
        else_branch,
    })
}

fn compile_a_expr(a: &pg_query::protobuf::AExpr) -> Result<Expr> {
    use pg_query::protobuf::AExprKind;
    let kind = a.kind();
    match kind {
        AExprKind::AexprOp => {
            let op_name = extract_strings(&a.name)?.join("");
            if a.lexpr.is_none() {
                let rhs = a
                    .rexpr
                    .as_ref()
                    .ok_or_else(|| SQLError::Internal("AExpr missing rhs".into()))?;
                let rhs = compile_expr(rhs)?;
                let unary_func = |name: &str, arg: Expr| Expr::Func {
                    name: name.into(),
                    args: vec![arg],
                    distinct: false,
                    order_by: Vec::new(),
                    filter: None,
                };
                return match op_name.as_str() {
                    "+" => Ok(rhs),
                    "-" => Ok(Expr::Binary {
                        op: BinaryOp::Subtract,
                        lhs: Box::new(Expr::Literal(Value::Int(0))),
                        rhs: Box::new(rhs),
                    }),
                    // |/ square root, ||/ cube root, @ absolute value.
                    "|/" => Ok(unary_func("sqrt", rhs)),
                    "||/" => Ok(unary_func("cbrt", rhs)),
                    "@" => Ok(unary_func("abs", rhs)),
                    other => Err(SQLError::Unsupported(format!("unary operator `{other}`"))),
                };
            }
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("AExpr missing rhs".into()))?;
            let op = match op_name.as_str() {
                "=" => BinaryOp::Equal,
                "<>" | "!=" => BinaryOp::NotEqual,
                "<" => BinaryOp::Less,
                "<=" => BinaryOp::LessEqual,
                ">" => BinaryOp::Greater,
                ">=" => BinaryOp::GreaterEqual,
                "+" => BinaryOp::Add,
                "-" => BinaryOp::Subtract,
                "*" => BinaryOp::Multiply,
                "/" => BinaryOp::Divide,
                // String concatenation: rewrite `a || b` into a
                // concat_op() call. concat_op propagates NULL the way
                // the SQL `||` operator must (`'x' || NULL == NULL`),
                // which is distinct from PostgreSQL's `CONCAT()` that
                // skips NULL arguments.
                "||" => {
                    return Ok(Expr::Func {
                        name: "concat_op".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@@" => {
                    return Ok(Expr::Func {
                        name: "fts_match".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@?" => {
                    return Ok(Expr::Func {
                        name: "jsonpath_exists".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "%" => {
                    return Ok(Expr::Func {
                        name: "mod".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "^" => {
                    return Ok(Expr::Func {
                        name: "power".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                // POSIX regex operators: `~` match, `~*` case-insensitive
                // match, `!~` / `!~*` their negations.
                "~" | "~*" | "!~" | "!~*" => {
                    let mut args = vec![compile_expr(lhs)?, compile_expr(rhs)?];
                    if op_name.ends_with('*') {
                        args.push(Expr::Literal(Value::Str("i".into())));
                    }
                    let call = Expr::Func {
                        name: "regexp_like".into(),
                        args,
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    };
                    return Ok(if op_name.starts_with('!') {
                        Expr::Not(Box::new(call))
                    } else {
                        call
                    });
                }
                // Array overlap.
                "&&" => {
                    return Ok(Expr::Func {
                        name: "array_overlap".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "~~" => {
                    return Ok(Expr::Func {
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "~~*" => {
                    return Ok(Expr::Func {
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "!~~" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        name: "like".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })));
                }
                "!~~*" => {
                    return Ok(Expr::Not(Box::new(Expr::Func {
                        name: "ilike".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    })));
                }
                "->" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "->>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path_text".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#>>" => {
                    return Ok(Expr::Func {
                        name: "json_extract_path_text".into(),
                        args: json_path_args(compile_expr(lhs)?, compile_expr(rhs)?),
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "#-" => {
                    return Ok(Expr::Func {
                        name: "json_delete_path".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "@>" => {
                    return Ok(Expr::Func {
                        name: "json_contains".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "<@" => {
                    return Ok(Expr::Func {
                        name: "json_contained_by".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?" => {
                    return Ok(Expr::Func {
                        name: "json_has_key".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?|" => {
                    return Ok(Expr::Func {
                        name: "json_has_any_key".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                "?&" => {
                    return Ok(Expr::Func {
                        name: "json_has_all_keys".into(),
                        args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                        distinct: false,
                        order_by: Vec::new(),
                        filter: None,
                    });
                }
                other => return Err(SQLError::Unsupported(format!("operator `{other}`"))),
            };
            Ok(Expr::Binary {
                op,
                lhs: Box::new(compile_expr(lhs)?),
                rhs: Box::new(compile_expr(rhs)?),
            })
        }
        AExprKind::AexprBetween | AExprKind::AexprNotBetween => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SQLError::Internal("BETWEEN expects 2 bounds".into())),
            };
            let between = Expr::Between {
                expr: Box::new(compile_expr(expr)?),
                low: Box::new(compile_expr(&bounds[0])?),
                high: Box::new(compile_expr(&bounds[1])?),
            };
            Ok(if matches!(kind, AExprKind::AexprNotBetween) {
                Expr::Not(Box::new(between))
            } else {
                between
            })
        }
        AExprKind::AexprBetweenSym | AExprKind::AexprNotBetweenSym => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SQLError::Internal("BETWEEN expects 2 bounds".into())),
            };
            let call = Expr::Func {
                name: "__between_symmetric".into(),
                args: vec![
                    compile_expr(expr)?,
                    compile_expr(&bounds[0])?,
                    compile_expr(&bounds[1])?,
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            Ok(if matches!(kind, AExprKind::AexprNotBetweenSym) {
                Expr::Not(Box::new(call))
            } else {
                call
            })
        }
        AExprKind::AexprDistinct | AExprKind::AexprNotDistinct => {
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IS DISTINCT FROM without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IS DISTINCT FROM without rhs".into()))?;
            let call = Expr::Func {
                name: "__is_distinct".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            Ok(if matches!(kind, AExprKind::AexprNotDistinct) {
                Expr::Not(Box::new(call))
            } else {
                call
            })
        }
        AExprKind::AexprSimilar => {
            // `expr SIMILAR TO pattern` arrives with the pattern
            // wrapped in `similar_to_escape(pattern[, escape])`.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without rhs".into()))?;
            let pattern = match rhs.node.as_ref() {
                Some(NodeEnum::FuncCall(f)) => {
                    let function_name = extract_strings(&f.funcname)?
                        .into_iter()
                        .next_back()
                        .ok_or_else(|| {
                            SQLError::Internal("SIMILAR TO wrapper function has no name".into())
                        })?;
                    if function_name != "similar_to_escape" {
                        return Err(SQLError::Internal(format!(
                            "SIMILAR TO has unexpected wrapper `{function_name}`"
                        )));
                    }
                    let [first] = f.args.as_slice() else {
                        if f.args.len() > 1 {
                            return Err(SQLError::Unsupported(
                                "SIMILAR TO with an explicit ESCAPE is not supported".into(),
                            ));
                        }
                        return Err(SQLError::Internal(
                            "similar_to_escape without pattern".into(),
                        ));
                    };
                    if f.agg_distinct
                        || f.agg_star
                        || f.agg_within_group
                        || f.func_variadic
                        || !f.agg_order.is_empty()
                        || f.agg_filter.is_some()
                        || f.over.is_some()
                    {
                        return Err(SQLError::Internal(
                            "SIMILAR TO wrapper contains aggregate/function modifiers".into(),
                        ));
                    }
                    compile_expr(first)?
                }
                _ => compile_expr(rhs)?,
            };
            let call = Expr::Func {
                name: "similar_to".into(),
                args: vec![compile_expr(lhs)?, pattern],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            match op_name.as_str() {
                "~" => Ok(call),
                "!~" => Ok(Expr::Not(Box::new(call))),
                other => Err(SQLError::Internal(format!(
                    "SIMILAR TO has unexpected operator `{other}`"
                ))),
            }
        }
        AExprKind::AexprOpAny | AExprKind::AexprOpAll => {
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ANY/ALL without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ANY/ALL without rhs".into()))?;
            let name = if matches!(kind, AExprKind::AexprOpAny) {
                "__any_op"
            } else {
                "__all_op"
            };
            Ok(Expr::Func {
                name: name.into(),
                args: vec![
                    compile_expr(lhs)?,
                    compile_expr(rhs)?,
                    Expr::Literal(Value::Str(op_name)),
                ],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            })
        }
        AExprKind::AexprNullif => {
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("NULLIF without rhs".into()))?;
            return Ok(Expr::Func {
                name: "nullif".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            });
        }
        AExprKind::AexprLike => {
            // libpg_query encodes LIKE as `~~` and NOT LIKE as `!~~` in
            // `a.name`. The keyword form lands here regardless of the
            // user's syntax (LIKE / NOT LIKE / ~~ / !~~), so we have to
            // peek at the name to recover the negation.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without rhs".into()))?;
            if let Some(NodeEnum::FuncCall(f)) = rhs.node.as_ref() {
                let wrapper = extract_strings(&f.funcname)?
                    .into_iter()
                    .next_back()
                    .ok_or_else(|| SQLError::Internal("LIKE wrapper has no name".into()))?;
                if wrapper == "like_escape" {
                    return Err(SQLError::Unsupported(
                        "LIKE with an explicit ESCAPE is not supported".into(),
                    ));
                }
            }
            let func = Expr::Func {
                name: "like".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return match op_name.as_str() {
                "~~" => Ok(func),
                "!~~" => Ok(Expr::Not(Box::new(func))),
                other => Err(SQLError::Internal(format!(
                    "LIKE has unexpected operator `{other}`"
                ))),
            };
        }
        AExprKind::AexprIlike => {
            // Same shape as AexprLike: ILIKE -> `~~*`, NOT ILIKE -> `!~~*`.
            let op_name = extract_strings(&a.name)?.join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without rhs".into()))?;
            if let Some(NodeEnum::FuncCall(f)) = rhs.node.as_ref() {
                let wrapper = extract_strings(&f.funcname)?
                    .into_iter()
                    .next_back()
                    .ok_or_else(|| SQLError::Internal("ILIKE wrapper has no name".into()))?;
                if wrapper == "like_escape" {
                    return Err(SQLError::Unsupported(
                        "ILIKE with an explicit ESCAPE is not supported".into(),
                    ));
                }
            }
            let func = Expr::Func {
                name: "ilike".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return match op_name.as_str() {
                "~~*" => Ok(func),
                "!~~*" => Ok(Expr::Not(Box::new(func))),
                other => Err(SQLError::Internal(format!(
                    "ILIKE has unexpected operator `{other}`"
                ))),
            };
        }
        AExprKind::AexprIn => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("IN without rhs".into()))?;
            let items = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) => l.items.clone(),
                _ => return Err(SQLError::Internal("IN expects list".into())),
            };
            let list: Vec<Expr> = items.iter().map(compile_expr).collect::<Result<Vec<_>>>()?;
            let operator = extract_strings(&a.name)?.join("");
            let negated = match operator.as_str() {
                "=" => false,
                "<>" => true,
                other => {
                    return Err(SQLError::Internal(format!(
                        "IN has unexpected operator `{other}`"
                    )));
                }
            };
            Ok(Expr::InList {
                expr: Box::new(compile_expr(expr)?),
                list,
                negated,
            })
        }
        other => Err(SQLError::Unsupported(format!("AExpr kind: {other:?}"))),
    }
}

fn compile_bool_expr(b: &pg_query::protobuf::BoolExpr) -> Result<Expr> {
    use pg_query::protobuf::BoolExprType;
    let kind = b.boolop();
    let args: Vec<Expr> = b
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    match kind {
        BoolExprType::AndExpr if args.len() >= 2 => Ok(Expr::And(args)),
        BoolExprType::OrExpr if args.len() >= 2 => Ok(Expr::Or(args)),
        BoolExprType::AndExpr | BoolExprType::OrExpr => Err(SQLError::Internal(format!(
            "{kind:?} requires at least two operands, got {}",
            args.len()
        ))),
        BoolExprType::NotExpr => {
            let [arg] = args.as_slice() else {
                return Err(SQLError::Internal(format!(
                    "NOT requires exactly one operand, got {}",
                    args.len()
                )));
            };
            Ok(Expr::Not(Box::new(arg.clone())))
        }
        _ => Err(SQLError::Unsupported(format!("BoolExpr {kind:?}"))),
    }
}

fn compile_null_test(n: &pg_query::protobuf::NullTest) -> Result<Expr> {
    use pg_query::protobuf::NullTestType;
    let arg = n
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("NullTest without arg".into()))?;
    let negated = match n.nulltesttype() {
        NullTestType::IsNull => false,
        NullTestType::IsNotNull => true,
        other => {
            return Err(SQLError::Internal(format!(
                "NullTest has invalid kind {other:?}"
            )));
        }
    };
    Ok(Expr::IsNull {
        expr: Box::new(compile_expr(arg)?),
        negated,
    })
}

fn compile_const(c: &pg_query::protobuf::AConst) -> Result<Expr> {
    if c.isnull {
        if c.val.is_some() {
            return Err(SQLError::Internal(
                "NULL constant unexpectedly has a value payload".into(),
            ));
        }
        return Ok(Expr::Literal(Value::Null));
    }
    use pg_query::protobuf::a_const::Val;
    let Some(val) = c.val.as_ref() else {
        return Err(SQLError::Internal(
            "non-NULL constant has no value payload".into(),
        ));
    };
    let value = match val {
        Val::Ival(i) => Value::Int(i64::from(i.ival)),
        // Integer literals wider than int4 arrive as Fval strings (the
        // parser also folds a unary minus into the literal); ones that
        // fit i64 stay integers (PostgreSQL types them int8), the rest
        // become numeric.
        Val::Fval(f)
            if f.fval
                .strip_prefix('-')
                .unwrap_or(&f.fval)
                .bytes()
                .all(|b| b.is_ascii_digit()) =>
        {
            f.fval.parse::<i64>().map(Value::Int).or_else(|_| {
                DecimalValue::parse(&f.fval)
                    .map(Value::Decimal)
                    .ok_or_else(|| SQLError::Internal(format!("bad numeric literal {}", f.fval)))
            })?
        }
        Val::Fval(f) => DecimalValue::parse(&f.fval).map_or_else(
            || {
                f.fval
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|e| SQLError::Internal(e.to_string()))
            },
            |d| Ok(Value::Decimal(d)),
        )?,
        Val::Sval(s) => Value::Str(s.sval.clone()),
        Val::Boolval(b) => Value::Bool(b.boolval),
        other => {
            return Err(SQLError::Unsupported(format!("constant: {other:?}")));
        }
    };
    Ok(Expr::Literal(value))
}

fn compile_column_ref(c: &pg_query::protobuf::ColumnRef) -> Result<Expr> {
    let mut parts: Vec<String> = Vec::new();
    for (index, f) in c.fields.iter().enumerate() {
        let inner = f
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("ColumnRef contains an empty field".into()))?;
        match inner {
            NodeEnum::String(s) if !s.sval.is_empty() => parts.push(s.sval.clone()),
            NodeEnum::String(_) => {
                return Err(SQLError::Internal(
                    "ColumnRef contains an empty name component".into(),
                ));
            }
            NodeEnum::AStar(_) if c.fields.len() == 1 && index == 0 => return Ok(Expr::Star),
            NodeEnum::AStar(_) => {
                return Err(SQLError::Unsupported(
                    "qualified wildcard projections are not represented by Expr::Star".into(),
                ));
            }
            other => {
                return Err(SQLError::Internal(format!(
                    "ColumnRef contains unexpected field {other:?}"
                )));
            }
        }
    }
    match parts.len() {
        0 => Err(SQLError::Internal("empty ColumnRef".into())),
        1 => Ok(Expr::Column(parts.pop().ok_or_else(|| {
            SQLError::Internal("ColumnRef component disappeared during lowering".into())
        })?)),
        _ => {
            // `schema.table.col` collapses to `table.col`; `t.col`
            // round-trips as a qualified ref.
            let column = parts.pop().ok_or_else(|| {
                SQLError::Internal("ColumnRef column disappeared during lowering".into())
            })?;
            let qualifier = parts.pop().ok_or_else(|| {
                SQLError::Internal("ColumnRef qualifier disappeared during lowering".into())
            })?;
            Ok(Expr::qualified_column(qualifier, column))
        }
    }
}

fn compile_func_call(f: &pg_query::protobuf::FuncCall) -> Result<Expr> {
    let raw_name = compile_qualified_name(&f.funcname, "function call")?;
    if raw_name.is_empty() {
        return Err(SQLError::Internal("function call has an empty name".into()));
    }
    if f.func_variadic {
        return Err(SQLError::Unsupported(format!(
            "VARIADIC invocation of `{raw_name}` is not represented by Expr::Func"
        )));
    }
    let mut args = f
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if f.agg_star {
        if !args.is_empty() {
            return Err(SQLError::Internal(format!(
                "function `{raw_name}` has both `*` and explicit arguments"
            )));
        }
        if f.agg_distinct || !f.agg_order.is_empty() || f.agg_within_group {
            return Err(SQLError::Internal(format!(
                "function `{raw_name}(*)` has incompatible aggregate modifiers"
            )));
        }
        args.push(Expr::Star);
    }
    if f.agg_within_group && f.agg_order.is_empty() {
        return Err(SQLError::Internal(format!(
            "ordered-set aggregate `{raw_name}` has no WITHIN GROUP ordering"
        )));
    }
    if let Some(over) = f.over.as_ref() {
        if f.agg_filter.is_some() || !f.agg_order.is_empty() || f.agg_distinct || f.agg_within_group
        {
            return Err(SQLError::Unsupported(format!(
                "window call `{raw_name}` uses aggregate modifiers not represented by WindowCall"
            )));
        }
        let spec = compile_window_spec(over)?;
        return Ok(Expr::WindowCall {
            name: raw_name,
            args,
            spec,
        });
    }
    // Translate the aggregate's ORDER BY clauses (e.g.
    // `string_agg(name, ',' ORDER BY name)`) into typed `OrderBy`
    // entries on `Expr::Func.order_by`.
    let mut agg_order: Vec<OrderBy> = Vec::new();
    for sort_node in &f.agg_order {
        let inner = sort_node.node.as_ref().ok_or_else(|| {
            SQLError::Internal("aggregate ORDER BY contains an empty item".into())
        })?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "aggregate ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("agg_order SortBy without expr".into()))?;
        let key_expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "aggregate ORDER BY")?;
        agg_order.push(OrderBy {
            expr: key_expr,
            descending,
            nulls,
        });
    }
    let agg_filter = match f.agg_filter.as_ref() {
        Some(inner) => Some(Box::new(compile_expr(inner)?)),
        None => None,
    };
    Ok(Expr::Func {
        name: raw_name,
        args,
        distinct: f.agg_distinct,
        order_by: agg_order,
        filter: agg_filter,
    })
}

fn compile_window_spec(w: &pg_query::protobuf::WindowDef) -> Result<WindowSpec> {
    if !w.name.is_empty() || !w.refname.is_empty() {
        return Err(SQLError::Unsupported(
            "named window references are not represented by WindowSpec".into(),
        ));
    }
    let partition_by: Vec<Expr> = w
        .partition_clause
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    let mut order_by = Vec::new();
    for sort_node in &w.order_clause {
        let inner = sort_node
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("window ORDER BY contains an empty item".into()))?;
        let NodeEnum::SortBy(sb) = inner else {
            return Err(SQLError::Internal(format!(
                "window ORDER BY expected SortBy, got {inner:?}"
            )));
        };
        let expr_node = sb
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
        let expr = compile_expr(expr_node)?;
        let (descending, nulls) = compile_sort_options(sb, "window ORDER BY")?;
        order_by.push(OrderBy {
            expr,
            descending,
            nulls,
        });
    }
    let frame = compile_window_frame(w)?;
    Ok(WindowSpec {
        partition_by,
        order_by,
        frame,
    })
}

fn compile_window_frame(
    w: &pg_query::protobuf::WindowDef,
) -> Result<Option<crate::ast::WindowFrame>> {
    use crate::ast::{FrameBound, FrameMode, WindowFrame};
    // pg_query bit constants for frame_options.
    const FRAMEOPTION_NONDEFAULT: u32 = 0x000_0001;
    const FRAMEOPTION_RANGE: u32 = 0x000_0002;
    const FRAMEOPTION_ROWS: u32 = 0x000_0004;
    const FRAMEOPTION_GROUPS: u32 = 0x000_0008;
    const FRAMEOPTION_BETWEEN: u32 = 0x000_0010;
    const FRAMEOPTION_START_UNBOUNDED_PRECEDING: u32 = 0x000_0020;
    const FRAMEOPTION_END_UNBOUNDED_PRECEDING: u32 = 0x000_0040;
    const FRAMEOPTION_START_UNBOUNDED_FOLLOWING: u32 = 0x000_0080;
    const FRAMEOPTION_END_UNBOUNDED_FOLLOWING: u32 = 0x000_0100;
    const FRAMEOPTION_START_CURRENT_ROW: u32 = 0x000_0200;
    const FRAMEOPTION_END_CURRENT_ROW: u32 = 0x000_0400;
    const FRAMEOPTION_START_OFFSET_PRECEDING: u32 = 0x000_0800;
    const FRAMEOPTION_END_OFFSET_PRECEDING: u32 = 0x000_1000;
    const FRAMEOPTION_START_OFFSET_FOLLOWING: u32 = 0x000_2000;
    const FRAMEOPTION_END_OFFSET_FOLLOWING: u32 = 0x000_4000;
    const FRAMEOPTION_EXCLUDE_CURRENT_ROW: u32 = 0x000_8000;
    const FRAMEOPTION_EXCLUDE_GROUP: u32 = 0x001_0000;
    const FRAMEOPTION_EXCLUDE_TIES: u32 = 0x002_0000;
    const FRAMEOPTION_EXCLUSION: u32 =
        FRAMEOPTION_EXCLUDE_CURRENT_ROW | FRAMEOPTION_EXCLUDE_GROUP | FRAMEOPTION_EXCLUDE_TIES;
    const KNOWN_OPTIONS: u32 = FRAMEOPTION_NONDEFAULT
        | FRAMEOPTION_RANGE
        | FRAMEOPTION_ROWS
        | FRAMEOPTION_GROUPS
        | FRAMEOPTION_BETWEEN
        | FRAMEOPTION_START_UNBOUNDED_PRECEDING
        | FRAMEOPTION_END_UNBOUNDED_PRECEDING
        | FRAMEOPTION_START_UNBOUNDED_FOLLOWING
        | FRAMEOPTION_END_UNBOUNDED_FOLLOWING
        | FRAMEOPTION_START_CURRENT_ROW
        | FRAMEOPTION_END_CURRENT_ROW
        | FRAMEOPTION_START_OFFSET_PRECEDING
        | FRAMEOPTION_END_OFFSET_PRECEDING
        | FRAMEOPTION_START_OFFSET_FOLLOWING
        | FRAMEOPTION_END_OFFSET_FOLLOWING
        | FRAMEOPTION_EXCLUSION;
    let f = u32::try_from(w.frame_options).map_err(|_| {
        SQLError::Internal(format!(
            "window frame options cannot be negative: {}",
            w.frame_options
        ))
    })?;
    let unknown = f & !KNOWN_OPTIONS;
    if unknown != 0 {
        return Err(SQLError::Internal(format!(
            "window frame contains unknown option bits 0x{unknown:x}"
        )));
    }
    if f & FRAMEOPTION_EXCLUSION != 0 {
        return Err(SQLError::Unsupported(
            "window frame EXCLUDE clauses are not represented by WindowFrame".into(),
        ));
    }
    // PostgreSQL always encodes a default frame in `frame_options`
    // (RANGE UNBOUNDED PRECEDING TO CURRENT ROW). Only honor the
    // frame when the user explicitly wrote one - that's exactly what
    // the `FRAMEOPTION_NONDEFAULT` bit indicates.
    if f & FRAMEOPTION_NONDEFAULT == 0 {
        if w.start_offset.is_some() || w.end_offset.is_some() {
            return Err(SQLError::Internal(
                "default window frame unexpectedly has an offset expression".into(),
            ));
        }
        return Ok(None);
    }
    let mode_bits = f & (FRAMEOPTION_RANGE | FRAMEOPTION_ROWS | FRAMEOPTION_GROUPS);
    let mode = match mode_bits {
        FRAMEOPTION_RANGE => FrameMode::Range,
        FRAMEOPTION_ROWS => FrameMode::Rows,
        FRAMEOPTION_GROUPS => FrameMode::Groups,
        other => {
            return Err(SQLError::Internal(format!(
                "window frame must select exactly one mode, got bits 0x{other:x}"
            )));
        }
    };
    let start_bits = f
        & (FRAMEOPTION_START_UNBOUNDED_PRECEDING
            | FRAMEOPTION_START_UNBOUNDED_FOLLOWING
            | FRAMEOPTION_START_CURRENT_ROW
            | FRAMEOPTION_START_OFFSET_PRECEDING
            | FRAMEOPTION_START_OFFSET_FOLLOWING);
    if start_bits.count_ones() != 1 {
        return Err(SQLError::Internal(format!(
            "window frame must select exactly one start bound, got bits 0x{start_bits:x}"
        )));
    }
    let end_bits = f
        & (FRAMEOPTION_END_UNBOUNDED_PRECEDING
            | FRAMEOPTION_END_UNBOUNDED_FOLLOWING
            | FRAMEOPTION_END_CURRENT_ROW
            | FRAMEOPTION_END_OFFSET_PRECEDING
            | FRAMEOPTION_END_OFFSET_FOLLOWING);
    if end_bits.count_ones() != 1 {
        return Err(SQLError::Internal(format!(
            "window frame must select exactly one end bound, got bits 0x{end_bits:x}"
        )));
    }
    let start = if f & FRAMEOPTION_START_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_START_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_START_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_START_OFFSET_PRECEDING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_START_OFFSET_FOLLOWING != 0 {
        let n = w
            .start_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        return Err(SQLError::Internal(
            "window frame start bound was not recognized".into(),
        ));
    };
    let end = if f & FRAMEOPTION_END_UNBOUNDED_PRECEDING != 0 {
        FrameBound::UnboundedPreceding
    } else if f & FRAMEOPTION_END_UNBOUNDED_FOLLOWING != 0 {
        FrameBound::UnboundedFollowing
    } else if f & FRAMEOPTION_END_CURRENT_ROW != 0 {
        FrameBound::CurrentRow
    } else if f & FRAMEOPTION_END_OFFSET_PRECEDING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("PRECEDING without offset".into()))?;
        FrameBound::Preceding(Box::new(compile_expr(n)?))
    } else if f & FRAMEOPTION_END_OFFSET_FOLLOWING != 0 {
        let n = w
            .end_offset
            .as_deref()
            .ok_or_else(|| SQLError::Internal("FOLLOWING without offset".into()))?;
        FrameBound::Following(Box::new(compile_expr(n)?))
    } else {
        return Err(SQLError::Internal(
            "window frame end bound was not recognized".into(),
        ));
    };
    let start_uses_offset =
        f & (FRAMEOPTION_START_OFFSET_PRECEDING | FRAMEOPTION_START_OFFSET_FOLLOWING) != 0;
    if start_uses_offset != w.start_offset.is_some() {
        return Err(SQLError::Internal(
            "window frame start offset payload does not match its option bits".into(),
        ));
    }
    let end_uses_offset =
        f & (FRAMEOPTION_END_OFFSET_PRECEDING | FRAMEOPTION_END_OFFSET_FOLLOWING) != 0;
    if end_uses_offset != w.end_offset.is_some() {
        return Err(SQLError::Internal(
            "window frame end offset payload does not match its option bits".into(),
        ));
    }
    Ok(Some(WindowFrame { mode, start, end }))
}

fn compile_type_cast(tc: &pg_query::protobuf::TypeCast) -> Result<Expr> {
    let arg = tc
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without arg".into()))?;
    let inner = compile_expr(arg)?;
    let type_name = tc
        .type_name
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without a target type".into()))?;
    let raw_names = extract_strings(&type_name.names)?;
    // libpg_query reports built-in types qualified as `pg_catalog.<name>`;
    // peel the schema off so the evaluator only ever sees the bare type
    // and treat aliases (`int4` -> `integer`, `float8` -> `double
    // precision`) up front.
    let mut ty = raw_names
        .last()
        .ok_or_else(|| SQLError::Internal("TypeCast target has no name components".into()))?
        .to_lowercase();
    ty = match ty.as_str() {
        "int2" => "smallint".to_string(),
        "int4" => "integer".to_string(),
        "int8" => "bigint".to_string(),
        "float4" => "real".to_string(),
        "float8" => "double precision".to_string(),
        _ => ty,
    };
    // Carry length / precision modifiers (`varchar(1)`, `numeric(10,2)`)
    // so the evaluator can truncate / rescale like PostgreSQL.
    if matches!(
        ty.as_str(),
        "varchar" | "bpchar" | "char" | "character" | "character varying" | "numeric" | "decimal"
    ) {
        let mods = type_name
            .typmods
            .iter()
            .map(|node| match node.node.as_ref() {
                Some(NodeEnum::AConst(constant)) => match constant.val.as_ref() {
                    Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                        Ok(value.ival.to_string())
                    }
                    other => Err(SQLError::TypeMismatch(format!(
                        "type modifier must be an integer constant, got {other:?}"
                    ))),
                },
                other => Err(SQLError::TypeMismatch(format!(
                    "type modifier must be an integer constant, got {other:?}"
                ))),
            })
            .collect::<Result<Vec<_>>>()?;
        if !mods.is_empty() {
            ty = format!("{ty}({})", mods.join(","));
        }
    }
    if !type_name.array_bounds.is_empty() && !ty.ends_with("[]") {
        ty.push_str("[]");
    }
    Ok(Expr::Cast {
        expr: Box::new(inner),
        ty,
    })
}

#[cfg(test)]
mod malformed_tree_tests {
    use super::*;

    fn int_node(value: i32) -> Node {
        Node {
            node: Some(NodeEnum::AConst(pg_query::protobuf::AConst {
                val: Some(pg_query::protobuf::a_const::Val::Ival(
                    pg_query::protobuf::Integer { ival: value },
                )),
                ..Default::default()
            })),
        }
    }

    #[test]
    fn set_operation_requires_both_children() {
        let missing_left = pg_query::protobuf::SelectStmt {
            op: pg_query::protobuf::SetOperation::SetopUnion as i32,
            rarg: Some(Box::default()),
            ..Default::default()
        };
        let error = compile_set_op(&missing_left).unwrap_err();
        assert!(matches!(
            error,
            SQLError::Internal(message) if message.contains("missing left")
        ));

        let missing_right = pg_query::protobuf::SelectStmt {
            op: pg_query::protobuf::SetOperation::SetopUnion as i32,
            larg: Some(Box::default()),
            ..Default::default()
        };
        let error = compile_set_op(&missing_right).unwrap_err();
        assert!(matches!(
            error,
            SQLError::Internal(message) if message.contains("missing right")
        ));
    }

    #[test]
    fn malformed_scalar_nodes_never_fall_back_to_null_or_default_semantics() {
        let empty_constant = pg_query::protobuf::AConst::default();
        assert!(matches!(
            compile_const(&empty_constant),
            Err(SQLError::Internal(message)) if message.contains("no value payload")
        ));

        let zero_parameter = Node {
            node: Some(NodeEnum::ParamRef(pg_query::protobuf::ParamRef::default())),
        };
        assert!(matches!(
            compile_expr(&zero_parameter),
            Err(SQLError::Internal(message)) if message.contains("greater than zero")
        ));

        let invalid_null_test = pg_query::protobuf::NullTest {
            arg: Some(Box::new(int_node(1))),
            ..Default::default()
        };
        assert!(matches!(
            compile_null_test(&invalid_null_test),
            Err(SQLError::Internal(message)) if message.contains("invalid kind")
        ));

        let malformed_not = pg_query::protobuf::BoolExpr {
            boolop: pg_query::protobuf::BoolExprType::NotExpr as i32,
            args: vec![int_node(1), int_node(2)],
            ..Default::default()
        };
        assert!(matches!(
            compile_bool_expr(&malformed_not),
            Err(SQLError::Internal(message)) if message.contains("exactly one")
        ));
    }

    #[test]
    fn malformed_sort_and_window_flags_are_rejected() {
        let undefined_sort = pg_query::protobuf::SortBy::default();
        assert!(matches!(
            compile_sort_options(&undefined_sort, "test ORDER BY"),
            Err(SQLError::Internal(message)) if message.contains("undefined sort direction")
        ));

        let negative_frame = pg_query::protobuf::WindowDef {
            frame_options: -1,
            ..Default::default()
        };
        assert!(matches!(
            compile_window_frame(&negative_frame),
            Err(SQLError::Internal(message)) if message.contains("cannot be negative")
        ));

        let exclusion_frame = pg_query::protobuf::WindowDef {
            frame_options: 0x000_0001 | 0x000_0004 | 0x000_0020 | 0x000_0400 | 0x000_8000,
            ..Default::default()
        };
        assert!(matches!(
            compile_window_frame(&exclusion_frame),
            Err(SQLError::Unsupported(message)) if message.contains("EXCLUDE")
        ));
    }

    #[test]
    fn unsupported_expression_shapes_fail_instead_of_losing_semantics() {
        for (sql, expected) in [
            ("SELECT source.* FROM source", "qualified wildcard"),
            ("SELECT 'abc' LIKE 'a%' ESCAPE '!'", "explicit ESCAPE"),
            (
                "SELECT 2 > ANY (SELECT value FROM values_table)",
                "ANY subquery operator",
            ),
            (
                "SELECT count(*) FILTER (WHERE true) OVER ()",
                "aggregate modifiers",
            ),
            (
                "SELECT sum(value) OVER (ORDER BY value ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) FROM values_table",
                "EXCLUDE",
            ),
            ("SELECT 1 ORDER BY 1 USING >", "USING operators"),
        ] {
            let error = crate::compile(sql).expect_err(sql);
            assert!(
                matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
                "unexpected error for {sql}: {error}"
            );
        }
    }

    #[test]
    fn type_modifiers_are_checked_without_numeric_truncation() {
        for sql in [
            "CREATE TABLE bad_vector (embedding vector(-1))",
            "CREATE TABLE zero_vector (embedding vector(0))",
            "CREATE TABLE extra_vector (embedding vector(2, 3))",
            "CREATE TABLE extra_numeric (amount numeric(10, 2, 1))",
        ] {
            assert!(crate::compile(sql).is_err(), "unexpected success for {sql}");
        }
    }
}
