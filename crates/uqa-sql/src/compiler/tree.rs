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
    Projection, SelectStmt, SetOp, SetOpKind, WindowSpec, CTE,
};
use crate::error::{Result, SQLError};

use super::range_var_name;
use super::types::{
    compile_foreign_key_action, compile_foreign_key_match, compile_type_name, raw_type_name,
    validate_foreign_key_set_columns,
};

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
    let name = stmt
        .relation
        .as_ref()
        .map(|r| {
            if r.schemaname.is_empty() {
                r.relname.clone()
            } else {
                format!("{}.{}", r.schemaname, r.relname)
            }
        })
        .unwrap_or_default();
    if name.is_empty() {
        return Err(SQLError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    let mut checks: Vec<TableCheck> = Vec::new();
    let mut foreign_keys: Vec<ForeignKey> = Vec::new();
    for elt in &stmt.table_elts {
        let Some(inner) = elt.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::ColumnDef(col) => {
                columns.push(compile_column_def(col)?);
            }
            NodeEnum::Constraint(cstr) => match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        let expr = compile_expr(raw)?;
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        checks.push(TableCheck { name: cname, expr });
                    }
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    let local_columns: Vec<String> = cstr
                        .fk_attrs
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect();
                    let ref_table = cstr
                        .pktable
                        .as_ref()
                        .map(|r| r.relname.clone())
                        .unwrap_or_default();
                    let ref_columns: Vec<String> = cstr
                        .pk_attrs
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect();
                    if !local_columns.is_empty() && !ref_table.is_empty() && !ref_columns.is_empty()
                    {
                        let cname = if cstr.conname.is_empty() {
                            None
                        } else {
                            Some(cstr.conname.clone())
                        };
                        let on_delete_set_columns: Vec<String> = cstr
                            .fk_del_set_cols
                            .iter()
                            .filter_map(|n| extract_string(n).ok())
                            .collect();
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
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(CreateTable {
        name,
        columns,
        if_not_exists: stmt.if_not_exists,
        checks,
        foreign_keys,
    })
}

pub(super) fn compile_column_def(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col).unwrap_or_default();
    let ty = compile_type_name(col)?;
    let mut auto_increment = matches!(raw_type.as_str(), "serial" | "bigserial");
    let mut primary_key = false;
    let mut not_null = false;
    let mut unique = false;
    let mut default: Option<Expr> = None;
    let mut check: Option<Expr> = None;
    let mut references: Option<crate::ast::ForeignKeyRef> = None;
    for c in &col.constraints {
        let Some(inner) = c.node.as_ref() else {
            continue;
        };
        if let NodeEnum::Constraint(cstr) = inner {
            match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => primary_key = true,
                pg_query::protobuf::ConstrType::ConstrNotnull => not_null = true,
                pg_query::protobuf::ConstrType::ConstrUnique => unique = true,
                pg_query::protobuf::ConstrType::ConstrIdentity => auto_increment = true,
                pg_query::protobuf::ConstrType::ConstrDefault => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        default = Some(compile_expr(raw)?);
                    }
                }
                pg_query::protobuf::ConstrType::ConstrCheck => {
                    if let Some(raw) = cstr.raw_expr.as_deref() {
                        check = Some(compile_expr(raw)?);
                    }
                }
                pg_query::protobuf::ConstrType::ConstrForeign => {
                    let table = cstr
                        .pktable
                        .as_ref()
                        .map(|r| r.relname.clone())
                        .unwrap_or_default();
                    let column = cstr
                        .pk_attrs
                        .iter()
                        .find_map(|n| extract_string(n).ok())
                        .unwrap_or_default();
                    if !table.is_empty() && !column.is_empty() {
                        references = Some(crate::ast::ForeignKeyRef {
                            table,
                            column,
                            on_update: compile_foreign_key_action(&cstr.fk_upd_action)?,
                            on_delete: compile_foreign_key_action(&cstr.fk_del_action)?,
                            match_type: compile_foreign_key_match(&cstr.fk_matchtype)?,
                        });
                    }
                }
                _ => {}
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
        .unwrap_or_default();
    let access_method = stmt.access_method.clone();
    let mut columns = Vec::new();
    for elt in &stmt.index_params {
        let Some(inner) = elt.node.as_ref() else {
            continue;
        };
        if let NodeEnum::IndexElem(idx) = inner {
            if !idx.name.is_empty() {
                columns.push(idx.name.clone());
            }
        }
    }
    let name = if stmt.idxname.is_empty() {
        None
    } else {
        Some(stmt.idxname.clone())
    };
    let mut options = Vec::new();
    for opt in &stmt.options {
        let Some(inner) = opt.node.as_ref() else {
            continue;
        };
        if let NodeEnum::DefElem(elem) = inner {
            let key = elem.defname.clone();
            let value = elem
                .arg
                .as_ref()
                .and_then(|n| n.node.as_ref())
                .map(|inner| match inner {
                    NodeEnum::String(s) => s.sval.clone(),
                    NodeEnum::Integer(i) => i.ival.to_string(),
                    NodeEnum::Float(f) => f.fval.clone(),
                    NodeEnum::TypeName(t) => t
                        .names
                        .iter()
                        .filter_map(|n| extract_string(n).ok())
                        .collect::<Vec<_>>()
                        .join("."),
                    other => format!("{other:?}"),
                })
                .unwrap_or_default();
            options.push((key, value));
        }
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
        .map(|r| r.relname.clone())
        .ok_or_else(|| SQLError::Internal("INSERT without relation".into()))?;
    let columns: Vec<String> = stmt
        .cols
        .iter()
        .filter_map(|c| {
            c.node.as_ref().and_then(|inner| match inner {
                NodeEnum::ResTarget(r) => Some(r.name.clone()),
                _ => None,
            })
        })
        .collect();
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
        let Some(inner) = row_node.node.as_ref() else {
            continue;
        };
        let list = match inner {
            NodeEnum::List(l) => l,
            _ => continue,
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

    let conflict_columns: Vec<String> = clause
        .infer
        .as_ref()
        .map(|infer| {
            infer
                .index_elems
                .iter()
                .filter_map(|elem| {
                    elem.node.as_ref().and_then(|inner| match inner {
                        NodeEnum::IndexElem(ie) => {
                            if ie.name.is_empty() {
                                None
                            } else {
                                Some(ie.name.clone())
                            }
                        }
                        _ => None,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let action = match clause.action() {
        PgAction::OnconflictNothing => OnConflictAction::Nothing,
        PgAction::OnconflictUpdate => {
            let mut assignments: Vec<(String, Expr)> = Vec::new();
            for tgt in &clause.target_list {
                let Some(inner) = tgt.node.as_ref() else {
                    continue;
                };
                let NodeEnum::ResTarget(rt) = inner else {
                    continue;
                };
                let Some(val) = rt.val.as_ref() else { continue };
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
        NodeEnum::RangeVar(r) => Ok(FromClause::Table {
            name: if r.schemaname.is_empty() {
                r.relname.clone()
            } else {
                format!("{}.{}", r.schemaname, r.relname)
            },
            alias: r.alias.as_ref().and_then(|a| {
                if a.aliasname.is_empty() {
                    None
                } else {
                    Some(a.aliasname.clone())
                }
            }),
        }),
        NodeEnum::JoinExpr(j) => {
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
            let (alias, column_aliases) = compile_alias(rs.alias.as_ref());
            let body_inner = body_node.node.as_ref().unwrap();
            if let NodeEnum::SelectStmt(s) = body_inner {
                if !s.values_lists.is_empty() {
                    let mut rows: Vec<Vec<Expr>> = Vec::new();
                    for r in &s.values_lists {
                        let Some(NodeEnum::List(list)) = r.node.as_ref() else {
                            continue;
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
            // The first function in `functions` carries the call. Take
            // that node verbatim and re-use compile_expr to lift it
            // into an Expr::Func, then peel back the name + args.
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
            let (alias, column_aliases) = compile_alias(rf.alias.as_ref());
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

fn compile_alias(alias: Option<&pg_query::protobuf::Alias>) -> (Option<String>, Vec<String>) {
    let Some(a) = alias else {
        return (None, Vec::new());
    };
    let name = if a.aliasname.is_empty() {
        None
    } else {
        Some(a.aliasname.clone())
    };
    let cols: Vec<String> = a
        .colnames
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    (name, cols)
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
                let type_name = col
                    .type_name
                    .as_ref()
                    .and_then(|t| t.names.last())
                    .and_then(|n| extract_string(n).ok())
                    .unwrap_or_default()
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
    let (projections, from, r#where, group_by, order_by, limit, offset) =
        if set_op.is_some() && stmt.larg.is_some() {
            // Promote the outer (combined) clauses onto the SetOp and
            // replace the parent's clauses with the LHS branch's.
            if let Some(so) = set_op.as_mut() {
                so.combined_order_by = order_by;
                so.combined_limit = limit;
                so.combined_offset = offset;
            }
            let lhs = compile_select(stmt.larg.as_deref().unwrap())?;
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
            Expr::Literal(Value::Int(n)) if *n >= 1 && (*n as usize) <= projections.len() => {
                projections[(*n as usize) - 1].expr.clone()
            }
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
    let mut plain: Vec<Expr> = Vec::new();
    let mut sets: Vec<Vec<Expr>> = Vec::new();
    let mut has_grouping_set = false;
    for n in nodes {
        let Some(inner) = n.node.as_ref() else {
            continue;
        };
        if let NodeEnum::GroupingSet(gs) = inner {
            has_grouping_set = true;
            let kind = gs.kind();
            // The content list holds either column refs or nested
            // GroupingSet nodes (for nested ROLLUP / CUBE).
            let inner_exprs: Vec<Expr> = gs
                .content
                .iter()
                .filter_map(|c| compile_expr(c).ok())
                .collect();
            match kind {
                GroupingSetKind::GroupingSetEmpty => {
                    sets.push(Vec::new());
                }
                GroupingSetKind::GroupingSetSimple => {
                    sets.push(inner_exprs);
                }
                GroupingSetKind::GroupingSetRollup => {
                    // ROLLUP(a, b, c) -> (a, b, c), (a, b), (a), ()
                    let n = inner_exprs.len();
                    for i in (0..=n).rev() {
                        sets.push(inner_exprs[..i].to_vec());
                    }
                }
                GroupingSetKind::GroupingSetCube => {
                    // CUBE(a, b) -> all 2^n subsets.
                    let n = inner_exprs.len();
                    for mask in 0_usize..(1 << n) {
                        let mut s: Vec<Expr> = Vec::new();
                        for (i, e) in inner_exprs.iter().enumerate() {
                            if mask & (1 << i) != 0 {
                                s.push(e.clone());
                            }
                        }
                        sets.push(s);
                    }
                }
                GroupingSetKind::GroupingSetSets => {
                    // Explicit GROUPING SETS ((a, b), (a), ()): every
                    // child of `content` is itself a GroupingSet.
                    for child in &gs.content {
                        if let Some(NodeEnum::GroupingSet(child_gs)) = child.node.as_ref() {
                            let exprs: Vec<Expr> = child_gs
                                .content
                                .iter()
                                .filter_map(|c| compile_expr(c).ok())
                                .collect();
                            sets.push(exprs);
                        }
                    }
                }
                _ => {}
            }
        } else {
            plain.push(compile_expr(n)?);
        }
    }
    if !has_grouping_set {
        return Ok((plain, Vec::new()));
    }
    // Standard plain group-by columns are AND-merged with every
    // grouping set: each set acquires the plain prefix.
    let merged: Vec<Vec<Expr>> = if plain.is_empty() {
        sets
    } else {
        sets.into_iter()
            .map(|s| {
                let mut combined = plain.clone();
                combined.extend(s);
                combined
            })
            .collect()
    };
    Ok((Vec::new(), merged))
}

pub(super) fn compile_projections(targets: &[pg_query::protobuf::Node]) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(targets.len());
    for target_node in targets {
        let Some(inner) = target_node.node.as_ref() else {
            continue;
        };
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
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            // SortByDir: SortbyDefault = 0, SortbyAsc = 2, SortbyDesc = 3,
            // SortbyUsing = 4 (per libpg_query 6.x).
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            // SortByNulls: SortbyNullsDefault = 0, SortbyNullsFirst = 1,
            // SortbyNullsLast = 2.
            // pg_query enum values: SortbyNullsDefault=1, First=2, Last=3.
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            out.push(OrderBy {
                expr,
                descending,
                nulls,
            });
        }
    }
    Ok(out)
}

fn compile_set_op(stmt: &pg_query::protobuf::SelectStmt) -> Result<Option<Box<SetOp>>> {
    let kind = match stmt.op() {
        pg_query::protobuf::SetOperation::SetopNone => return Ok(None),
        pg_query::protobuf::SetOperation::SetopUnion => SetOpKind::Union,
        pg_query::protobuf::SetOperation::SetopIntersect => SetOpKind::Intersect,
        pg_query::protobuf::SetOperation::SetopExcept => SetOpKind::Except,
        other => return Err(SQLError::Unsupported(format!("set op {other:?}"))),
    };
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
        let Some(inner) = cte_node.node.as_ref() else {
            continue;
        };
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SQLError::Internal("expected CommonTableExpr".into())),
        };
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
        let columns = cte
            .aliascolnames
            .iter()
            .filter_map(|n| extract_string(n).ok())
            .collect();
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
    let Some(inner) = node.node.as_ref() else {
        return Ok(None);
    };
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
        NodeEnum::ParamRef(p) => Ok(Expr::Param(p.number as usize)),
        NodeEnum::FuncCall(f) => compile_func_call(f),
        NodeEnum::NamedArgExpr(arg) => {
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
    for step in &ind.indirection {
        let Some(inner) = step.node.as_ref() else {
            continue;
        };
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
    match sl.sub_link_type() {
        SubLinkType::ExprSublink => Ok(Expr::ScalarSubquery(body)),
        SubLinkType::ExistsSublink => Ok(Expr::Exists {
            body,
            negated: false,
        }),
        SubLinkType::AnySublink => {
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
            // ALL is the negation of ANY <> for the dual operator. We
            // promote to InSubquery semantics with a clear marker; the
            // evaluator treats ALL like NOT IN for equality.
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
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
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
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SIMILAR TO without rhs".into()))?;
            let pattern = match rhs.node.as_ref() {
                Some(NodeEnum::FuncCall(f))
                    if f.funcname.iter().any(|n| {
                        matches!(n.node.as_ref(), Some(NodeEnum::String(s)) if s.sval == "similar_to_escape")
                    }) =>
                {
                    let first = f.args.first().ok_or_else(|| {
                        SQLError::Internal("similar_to_escape without pattern".into())
                    })?;
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
            Ok(if op_name == "!~" {
                Expr::Not(Box::new(call))
            } else {
                call
            })
        }
        AExprKind::AexprOpAny | AExprKind::AexprOpAll => {
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
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
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("LIKE without rhs".into()))?;
            let func = Expr::Func {
                name: "like".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return Ok(if op_name == "!~~" {
                Expr::Not(Box::new(func))
            } else {
                func
            });
        }
        AExprKind::AexprIlike => {
            // Same shape as AexprLike: ILIKE -> `~~*`, NOT ILIKE -> `!~~*`.
            let op_name = a
                .name
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect::<Vec<_>>()
                .join("");
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SQLError::Internal("ILIKE without rhs".into()))?;
            let func = Expr::Func {
                name: "ilike".into(),
                args: vec![compile_expr(lhs)?, compile_expr(rhs)?],
                distinct: false,
                order_by: Vec::new(),
                filter: None,
            };
            return Ok(if op_name == "!~~*" {
                Expr::Not(Box::new(func))
            } else {
                func
            });
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
            let negated = a
                .name
                .first()
                .and_then(|n| n.node.as_ref())
                .and_then(|inner| match inner {
                    NodeEnum::String(s) => Some(s.sval == "<>"),
                    _ => None,
                })
                .unwrap_or(false);
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
        BoolExprType::AndExpr => Ok(Expr::And(args)),
        BoolExprType::OrExpr => Ok(Expr::Or(args)),
        BoolExprType::NotExpr => {
            let arg = args
                .into_iter()
                .next()
                .ok_or_else(|| SQLError::Internal("NOT without operand".into()))?;
            Ok(Expr::Not(Box::new(arg)))
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
    let negated = matches!(n.nulltesttype(), NullTestType::IsNotNull);
    Ok(Expr::IsNull {
        expr: Box::new(compile_expr(arg)?),
        negated,
    })
}

fn compile_const(c: &pg_query::protobuf::AConst) -> Result<Expr> {
    if c.isnull {
        return Ok(Expr::Literal(Value::Null));
    }
    use pg_query::protobuf::a_const::Val;
    let Some(val) = c.val.as_ref() else {
        return Ok(Expr::Literal(Value::Null));
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
    for f in &c.fields {
        let Some(inner) = f.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::String(s) => parts.push(s.sval.clone()),
            NodeEnum::AStar(_) => return Ok(Expr::Star),
            _ => {}
        }
    }
    match parts.len() {
        0 => Err(SQLError::Internal("empty ColumnRef".into())),
        1 => Ok(Expr::Column(parts.pop().unwrap())),
        _ => {
            // `schema.table.col` collapses to `table.col`; `t.col`
            // round-trips as a qualified ref.
            let column = parts.pop().unwrap();
            let qualifier = parts.pop().unwrap();
            Ok(Expr::qualified_column(qualifier, column))
        }
    }
}

fn compile_func_call(f: &pg_query::protobuf::FuncCall) -> Result<Expr> {
    let raw_name = f
        .funcname
        .iter()
        .filter_map(|n| {
            n.node.as_ref().and_then(|inner| match inner {
                NodeEnum::String(s) => Some(s.sval.clone()),
                _ => None,
            })
        })
        .collect::<Vec<_>>()
        .last()
        .cloned()
        .unwrap_or_default();
    let mut args = f
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if let Some(over) = f.over.as_ref() {
        let spec = compile_window_spec(over)?;
        return Ok(Expr::WindowCall {
            name: raw_name,
            args,
            spec,
        });
    }
    // COUNT(*): the parser leaves `args` empty; mark explicitly so
    // the dispatcher distinguishes it from COUNT(column).
    if f.agg_star && args.is_empty() {
        args.push(Expr::Star);
    }
    // Translate the aggregate's ORDER BY clauses (e.g.
    // `string_agg(name, ',' ORDER BY name)`) into typed `OrderBy`
    // entries on `Expr::Func.order_by`.
    let mut agg_order: Vec<OrderBy> = Vec::new();
    for sort_node in &f.agg_order {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("agg_order SortBy without expr".into()))?;
            let key_expr = compile_expr(expr_node)?;
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            agg_order.push(OrderBy {
                expr: key_expr,
                descending,
                nulls,
            });
        }
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
    let partition_by: Vec<Expr> = w
        .partition_clause
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    let mut order_by = Vec::new();
    for sort_node in &w.order_clause {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SQLError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            // pg_query enum values: SortbyNullsDefault=1, First=2, Last=3.
            let nulls = match sb.sortby_nulls {
                2 => Some(crate::ast::NullsOrder::First),
                3 => Some(crate::ast::NullsOrder::Last),
                _ => None,
            };
            order_by.push(OrderBy {
                expr,
                descending,
                nulls,
            });
        }
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
    if w.frame_options == 0 {
        return Ok(None);
    }
    // pg_query bit constants for frame_options.
    const FRAMEOPTION_NONDEFAULT: u32 = 0x000_0001;
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
    let f = w.frame_options as u32;
    let _ = FRAMEOPTION_BETWEEN;
    // PostgreSQL always encodes a default frame in `frame_options`
    // (RANGE UNBOUNDED PRECEDING TO CURRENT ROW). Only honor the
    // frame when the user explicitly wrote one - that's exactly what
    // the `FRAMEOPTION_NONDEFAULT` bit indicates.
    if f & FRAMEOPTION_NONDEFAULT == 0 {
        return Ok(None);
    }
    let mode = if f & FRAMEOPTION_ROWS != 0 {
        FrameMode::Rows
    } else if f & FRAMEOPTION_GROUPS != 0 {
        FrameMode::Groups
    } else {
        // FRAMEOPTION_RANGE is the default mode bit when neither ROWS
        // nor GROUPS is set; an unset flag also defaults to RANGE.
        FrameMode::Range
    };
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
        FrameBound::UnboundedPreceding
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
        FrameBound::CurrentRow
    };
    Ok(Some(WindowFrame { mode, start, end }))
}

fn compile_type_cast(tc: &pg_query::protobuf::TypeCast) -> Result<Expr> {
    let arg = tc
        .arg
        .as_ref()
        .ok_or_else(|| SQLError::Internal("TypeCast without arg".into()))?;
    let inner = compile_expr(arg)?;
    let raw_names: Vec<String> = tc
        .type_name
        .as_ref()
        .map(|t| {
            t.names
                .iter()
                .filter_map(|n| extract_string(n).ok())
                .collect()
        })
        .unwrap_or_default();
    // libpg_query reports built-in types qualified as `pg_catalog.<name>`;
    // peel the schema off so the evaluator only ever sees the bare type
    // and treat aliases (`int4` -> `integer`, `float8` -> `double
    // precision`) up front.
    let mut ty = raw_names.last().cloned().unwrap_or_default().to_lowercase();
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
        let mods: Vec<String> = tc
            .type_name
            .as_ref()
            .map(|t| {
                t.typmods
                    .iter()
                    .filter_map(|node| match node.node.as_ref() {
                        Some(NodeEnum::AConst(c)) => match c.val.as_ref() {
                            Some(pg_query::protobuf::a_const::Val::Ival(i)) => {
                                Some(i.ival.to_string())
                            }
                            _ => None,
                        },
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !mods.is_empty() {
            ty = format!("{ty}({})", mods.join(","));
        }
    }
    if tc
        .type_name
        .as_ref()
        .is_some_and(|t| !t.array_bounds.is_empty())
        && !ty.ends_with("[]")
    {
        ty.push_str("[]");
    }
    if ty.is_empty() {
        return Ok(inner);
    }
    Ok(Expr::Cast {
        expr: Box::new(inner),
        ty,
    })
}
