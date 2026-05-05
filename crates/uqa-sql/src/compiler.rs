//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift a `pg_query` parse tree into the internal [`Statement`] AST.
//!
//! This is intentionally tight in scope: it covers `CREATE TABLE`,
//! `CREATE INDEX`, `INSERT`, and `SELECT` with the subset of clauses the
//! Phase 5 quickstart and parity fixture exercise. Anything outside that
//! grammar parses cleanly via `pg_query` but compiles to
//! [`SqlError::Unsupported`].

use pg_query::protobuf::Node;
use pg_query::NodeEnum;
use uqa_core::Value;

use crate::ast::{
    AlterTableAction, AlterTableStmt, BinaryOp, ColumnDef, ColumnType, CreateIndex, CreateTable,
    Cte, DeleteStmt, DropKind, DropStmt, Expr, FromClause, InsertStmt, JoinKind, OrderBy,
    Projection, SelectStmt, SetOp, SetOpKind, Statement, UpdateStmt, WindowSpec,
};
use crate::error::{Result, SqlError};

pub fn compile(sql: &str) -> Result<Vec<Statement>> {
    let parsed = pg_query::parse(sql)?;
    let mut out = Vec::with_capacity(parsed.protobuf.stmts.len());
    for raw in parsed.protobuf.stmts {
        let Some(node) = raw.stmt else { continue };
        out.push(compile_stmt(&node)?);
    }
    Ok(out)
}

fn compile_stmt(node: &Node) -> Result<Statement> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SqlError::Unsupported("empty statement".into()));
    };
    match inner {
        NodeEnum::CreateStmt(stmt) => compile_create_table(stmt).map(Statement::CreateTable),
        NodeEnum::IndexStmt(stmt) => compile_create_index(stmt).map(Statement::CreateIndex),
        NodeEnum::InsertStmt(stmt) => compile_insert(stmt).map(Statement::Insert),
        NodeEnum::SelectStmt(stmt) => compile_select(stmt).map(|s| Statement::Select(Box::new(s))),
        NodeEnum::UpdateStmt(stmt) => compile_update(stmt).map(Statement::Update),
        NodeEnum::DeleteStmt(stmt) => compile_delete(stmt).map(Statement::Delete),
        NodeEnum::DropStmt(stmt) => compile_drop(stmt).map(Statement::Drop),
        NodeEnum::AlterTableStmt(stmt) => compile_alter_table(stmt).map(Statement::AlterTable),
        NodeEnum::RenameStmt(stmt) => compile_rename(stmt).map(Statement::AlterTable),
        other => Err(SqlError::Unsupported(format!(
            "{}",
            other_node_label(other)
        ))),
    }
}

fn compile_update(stmt: &pg_query::protobuf::UpdateStmt) -> Result<UpdateStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SqlError::Internal("UPDATE without relation".into()))?;
    let mut assignments = Vec::new();
    for target_node in &stmt.target_list {
        let Some(inner) = target_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::ResTarget(rt) = inner {
            let value = rt
                .val
                .as_ref()
                .ok_or_else(|| SqlError::Internal("UPDATE assignment without value".into()))?;
            assignments.push((rt.name.clone(), compile_expr(value)?));
        }
    }
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    Ok(UpdateStmt {
        table,
        assignments,
        r#where,
    })
}

fn compile_delete(stmt: &pg_query::protobuf::DeleteStmt) -> Result<DeleteStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SqlError::Internal("DELETE without relation".into()))?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    Ok(DeleteStmt { table, r#where })
}

fn other_node_label(node: &NodeEnum) -> &'static str {
    match node {
        NodeEnum::ExplainStmt(_) => "EXPLAIN",
        NodeEnum::ViewStmt(_) => "CREATE VIEW",
        NodeEnum::TransactionStmt(_) => "BEGIN/COMMIT/ROLLBACK",
        NodeEnum::PrepareStmt(_) | NodeEnum::ExecuteStmt(_) => "PREPARE/EXECUTE",
        _ => "unknown statement",
    }
}

// -------------------------------------------------------------------------
// DROP TABLE / DROP INDEX [IF EXISTS] [CASCADE]
// -------------------------------------------------------------------------

fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<DropStmt> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectSchema => DropKind::Schema,
        other => {
            return Err(SqlError::Unsupported(format!(
                "DROP target {other:?} not supported"
            )));
        }
    };
    let mut names = Vec::new();
    for object in &stmt.objects {
        let Some(inner) = object.node.as_ref() else {
            continue;
        };
        match inner {
            NodeEnum::List(list) => {
                let parts: Vec<String> = list
                    .items
                    .iter()
                    .filter_map(|n| extract_string(n).ok())
                    .collect();
                if let Some(last) = parts.last() {
                    names.push(last.clone());
                }
            }
            NodeEnum::String(s) => names.push(s.sval.clone()),
            other => {
                return Err(SqlError::Unsupported(format!(
                    "DROP object node {other:?} not supported"
                )));
            }
        }
    }
    if names.is_empty() {
        return Err(SqlError::Internal("DROP without target name".into()));
    }
    let cascade = matches!(stmt.behavior(), DropBehavior::DropCascade);
    Ok(DropStmt {
        kind,
        names,
        if_exists: stmt.missing_ok,
        cascade,
    })
}

// -------------------------------------------------------------------------
// ALTER TABLE { ADD COLUMN | DROP COLUMN | RENAME COLUMN | RENAME TO }
// -------------------------------------------------------------------------

fn compile_alter_table(stmt: &pg_query::protobuf::AlterTableStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::{AlterTableType, DropBehavior};
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SqlError::Internal("ALTER TABLE without relation".into()))?;
    let if_exists = stmt.missing_ok;
    let cmd = stmt
        .cmds
        .first()
        .ok_or_else(|| SqlError::Internal("ALTER TABLE without command".into()))?;
    let inner = cmd
        .node
        .as_ref()
        .ok_or_else(|| SqlError::Internal("ALTER TABLE command body empty".into()))?;
    let cmd = match inner {
        NodeEnum::AlterTableCmd(c) => c,
        other => {
            return Err(SqlError::Unsupported(format!(
                "ALTER TABLE command {other:?}"
            )));
        }
    };
    let action = match cmd.subtype() {
        AlterTableType::AtAddColumn => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SqlError::Internal("ADD COLUMN without ColumnDef".into()))?;
            let col_def = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?,
                other => {
                    return Err(SqlError::Internal(format!(
                        "ADD COLUMN expected ColumnDef, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AddColumn {
                column: col_def,
                if_not_exists: cmd.missing_ok,
            }
        }
        AlterTableType::AtDropColumn => AlterTableAction::DropColumn {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
            cascade: matches!(cmd.behavior(), DropBehavior::DropCascade),
        },
        AlterTableType::AtAlterColumnType
        | AlterTableType::AtSetNotNull
        | AlterTableType::AtDropNotNull
        | AlterTableType::AtColumnDefault => {
            return Err(SqlError::Unsupported(format!(
                "ALTER COLUMN action {:?}",
                cmd.subtype()
            )));
        }
        other => {
            return Err(SqlError::Unsupported(format!(
                "ALTER TABLE action {other:?}"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists,
        action,
    })
}

fn compile_rename(stmt: &pg_query::protobuf::RenameStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::ObjectType;
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SqlError::Internal("RENAME without relation".into()))?;
    let action = match stmt.rename_type() {
        ObjectType::ObjectColumn => AlterTableAction::RenameColumn {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectTable => AlterTableAction::RenameTable {
            to: stmt.newname.clone(),
        },
        other => {
            return Err(SqlError::Unsupported(format!(
                "RENAME target {other:?} not supported"
            )));
        }
    };
    Ok(AlterTableStmt {
        table,
        if_exists: stmt.missing_ok,
        action,
    })
}

fn extract_string(node: &Node) -> Result<String> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SqlError::Internal("missing string node".into()));
    };
    match inner {
        NodeEnum::String(s) => Ok(s.sval.clone()),
        _ => Err(SqlError::Internal(format!(
            "expected String node, got {inner:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// CREATE TABLE
// -------------------------------------------------------------------------

fn compile_create_table(stmt: &pg_query::protobuf::CreateStmt) -> Result<CreateTable> {
    let name = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .unwrap_or_default();
    if name.is_empty() {
        return Err(SqlError::Internal("CREATE TABLE without name".into()));
    }
    let mut columns = Vec::new();
    for elt in &stmt.table_elts {
        let Some(inner) = elt.node.as_ref() else {
            continue;
        };
        if let NodeEnum::ColumnDef(col) = inner {
            columns.push(compile_column_def(col)?);
        }
    }
    Ok(CreateTable {
        name,
        columns,
        if_not_exists: stmt.if_not_exists,
    })
}

fn compile_column_def(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let raw_type = raw_type_name(col).unwrap_or_default();
    let ty = compile_type_name(col)?;
    let auto_increment = matches!(raw_type.as_str(), "serial" | "bigserial");
    let mut primary_key = false;
    let mut not_null = false;
    for c in &col.constraints {
        let Some(inner) = c.node.as_ref() else {
            continue;
        };
        if let NodeEnum::Constraint(cstr) = inner {
            match cstr.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => primary_key = true,
                pg_query::protobuf::ConstrType::ConstrNotnull => not_null = true,
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
    })
}

fn raw_type_name(col: &pg_query::protobuf::ColumnDef) -> Option<String> {
    let type_name = col.type_name.as_ref()?;
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    Some(names.last().cloned().unwrap_or_default().to_lowercase())
}

fn compile_type_name(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnType> {
    let Some(type_name) = col.type_name.as_ref() else {
        return Err(SqlError::Internal(format!(
            "column `{}` has no type",
            col.colname
        )));
    };
    let names: Vec<String> = type_name
        .names
        .iter()
        .filter_map(|n| extract_string(n).ok())
        .collect();
    let raw = names.last().cloned().unwrap_or_default().to_lowercase();
    match raw.as_str() {
        "int" | "int4" | "integer" | "bigint" | "int8" | "smallint" | "int2" | "serial"
        | "bigserial" | "serial4" | "serial8" => Ok(ColumnType::Integer),
        "text" | "varchar" | "character" | "char" | "bpchar" | "name" | "uuid" => {
            Ok(ColumnType::Text)
        }
        "bool" | "boolean" => Ok(ColumnType::Integer),
        "real" | "float4" | "float8" | "double" | "double precision" | "numeric" | "decimal" => {
            Ok(ColumnType::Real)
        }
        "date"
        | "time"
        | "timetz"
        | "timestamp"
        | "timestamptz"
        | "timestamp without time zone"
        | "timestamp with time zone"
        | "time without time zone"
        | "time with time zone" => Ok(ColumnType::Text),
        "json" | "jsonb" => Ok(ColumnType::Text),
        "vector" => {
            // VECTOR(N): the dimension is the only typmod argument.
            let Some(arg) = type_name.typmods.first() else {
                return Err(SqlError::Unsupported(
                    "VECTOR without dimension is not supported".into(),
                ));
            };
            let dim = expect_integer_const(arg)? as u32;
            Ok(ColumnType::Vector(dim))
        }
        other => Err(SqlError::Unsupported(format!(
            "column type `{other}` is not supported"
        ))),
    }
}

fn expect_integer_const(node: &Node) -> Result<i64> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SqlError::Internal("missing const node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => Ok(i64::from(i.ival)),
            Some(pg_query::protobuf::a_const::Val::Fval(f)) => f
                .fval
                .parse::<f64>()
                .map(|v| v as i64)
                .map_err(|e| SqlError::Internal(e.to_string())),
            other => Err(SqlError::Internal(format!(
                "expected integer constant, got {other:?}"
            ))),
        },
        _ => Err(SqlError::Internal(format!(
            "expected A_Const, got {inner:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// CREATE INDEX
// -------------------------------------------------------------------------

fn compile_create_index(stmt: &pg_query::protobuf::IndexStmt) -> Result<CreateIndex> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
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

fn compile_insert(stmt: &pg_query::protobuf::InsertStmt) -> Result<InsertStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(|r| r.relname.clone())
        .ok_or_else(|| SqlError::Internal("INSERT without relation".into()))?;
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
        .ok_or_else(|| SqlError::Unsupported("INSERT without VALUES".into()))?;
    let select_inner = select_node
        .node
        .as_ref()
        .ok_or_else(|| SqlError::Internal("INSERT select_stmt empty".into()))?;
    let select = match select_inner {
        NodeEnum::SelectStmt(s) => s,
        _ => return Err(SqlError::Unsupported("INSERT FROM SELECT".into())),
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
    Ok(InsertStmt {
        table,
        columns,
        rows,
    })
}

// -------------------------------------------------------------------------
// SELECT
// -------------------------------------------------------------------------

fn compile_from_node(node: &Node) -> Result<FromClause> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SqlError::Internal("empty FROM node".into()));
    };
    match inner {
        NodeEnum::RangeVar(r) => Ok(FromClause::Table {
            name: r.relname.clone(),
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
                .ok_or_else(|| SqlError::Internal("JOIN missing left".into()))?;
            let right = j
                .rarg
                .as_ref()
                .ok_or_else(|| SqlError::Internal("JOIN missing right".into()))?;
            let kind = match j.jointype() {
                pg_query::protobuf::JoinType::JoinInner => JoinKind::Inner,
                pg_query::protobuf::JoinType::JoinLeft => JoinKind::Left,
                pg_query::protobuf::JoinType::JoinRight => JoinKind::Right,
                pg_query::protobuf::JoinType::JoinFull => JoinKind::Full,
                other => {
                    return Err(SqlError::Unsupported(format!("JOIN type {other:?}")));
                }
            };
            let on = j.quals.as_deref().map(compile_expr).transpose()?;
            Ok(FromClause::Join {
                left: Box::new(compile_from_node(left)?),
                right: Box::new(compile_from_node(right)?),
                kind,
                on,
            })
        }
        other => Err(SqlError::Unsupported(format!("FROM form: {other:?}"))),
    }
}

fn compile_select(stmt: &pg_query::protobuf::SelectStmt) -> Result<SelectStmt> {
    let from = match stmt.from_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let projections = compile_projections(&stmt.target_list)?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let order_by = compile_order_by(&stmt.sort_clause)?;
    let limit = compile_int_const(stmt.limit_count.as_deref())?;
    let offset = compile_int_const(stmt.limit_offset.as_deref())?;
    let group_by: Vec<Expr> = stmt
        .group_clause
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    let set_op = compile_set_op(stmt)?;

    // For UNION shapes the LHS lives inside `larg`; pull projections /
    // FROM from there since the top-level node carries only the set op.
    let (projections, from, r#where, group_by, order_by, limit, offset) =
        if set_op.is_some() && stmt.larg.is_some() {
            let lhs = compile_select(stmt.larg.as_deref().unwrap())?;
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

    Ok(SelectStmt {
        projections,
        from,
        r#where,
        group_by,
        order_by,
        limit,
        offset,
        with,
        set_op,
    })
}

fn compile_projections(targets: &[pg_query::protobuf::Node]) -> Result<Vec<Projection>> {
    let mut out = Vec::with_capacity(targets.len());
    for target_node in targets {
        let Some(inner) = target_node.node.as_ref() else {
            continue;
        };
        let res_target = match inner {
            NodeEnum::ResTarget(t) => t,
            _ => return Err(SqlError::Internal(format!("unexpected target {inner:?}"))),
        };
        let alias = if res_target.name.is_empty() {
            None
        } else {
            Some(res_target.name.clone())
        };
        let expr = match &res_target.val {
            Some(node) => compile_expr(node)?,
            None => return Err(SqlError::Internal("ResTarget without value".into())),
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
                .ok_or_else(|| SqlError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            // SortByDir: SortbyDefault = 0, SortbyAsc = 2, SortbyDesc = 3,
            // SortbyUsing = 4 (per libpg_query 6.x).
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            out.push(OrderBy { expr, descending });
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
        other => return Err(SqlError::Unsupported(format!("set op {other:?}"))),
    };
    let right_node = stmt
        .rarg
        .as_deref()
        .ok_or_else(|| SqlError::Internal("set op missing right".into()))?;
    let right = compile_select(right_node)?;
    Ok(Some(Box::new(SetOp {
        kind,
        all: stmt.all,
        right,
    })))
}

fn compile_with_clause(wc: &pg_query::protobuf::WithClause) -> Result<Vec<Cte>> {
    let mut out = Vec::with_capacity(wc.ctes.len());
    for cte_node in &wc.ctes {
        let Some(inner) = cte_node.node.as_ref() else {
            continue;
        };
        let cte = match inner {
            NodeEnum::CommonTableExpr(c) => c,
            _ => return Err(SqlError::Internal("expected CommonTableExpr".into())),
        };
        let select_node = cte
            .ctequery
            .as_ref()
            .ok_or_else(|| SqlError::Internal("CTE without query".into()))?;
        let select_inner = select_node
            .node
            .as_ref()
            .ok_or_else(|| SqlError::Internal("CTE query node empty".into()))?;
        let select = match select_inner {
            NodeEnum::SelectStmt(s) => s,
            _ => return Err(SqlError::Unsupported("CTE body must be SELECT".into())),
        };
        out.push(Cte {
            name: cte.ctename.clone(),
            recursive: wc.recursive,
            query: Box::new(compile_select(select)?),
        });
    }
    Ok(out)
}

fn compile_int_const(node: Option<&Node>) -> Result<Option<u64>> {
    use pg_query::protobuf::a_const::Val;
    let Some(node) = node else { return Ok(None) };
    let Some(inner) = node.node.as_ref() else {
        return Ok(None);
    };
    match inner {
        NodeEnum::AConst(c) => match &c.val {
            Some(Val::Ival(i)) if i.ival >= 0 => Ok(Some(u64::from(i.ival as u32))),
            Some(Val::Ival(_)) => Err(SqlError::Internal("negative LIMIT/OFFSET".into())),
            None => Ok(None),
            other => Err(SqlError::Internal(format!(
                "non-integer LIMIT/OFFSET: {other:?}"
            ))),
        },
        _ => Err(SqlError::Internal(format!(
            "LIMIT/OFFSET expr not supported: {inner:?}"
        ))),
    }
}

// -------------------------------------------------------------------------
// Expression compiler
// -------------------------------------------------------------------------

fn compile_expr(node: &Node) -> Result<Expr> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SqlError::Internal("missing expr node".into()));
    };
    match inner {
        NodeEnum::AConst(c) => compile_const(c),
        NodeEnum::ColumnRef(c) => compile_column_ref(c),
        NodeEnum::ParamRef(p) => Ok(Expr::Param(p.number as usize)),
        NodeEnum::FuncCall(f) => compile_func_call(f),
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
        NodeEnum::BoolExpr(b) => compile_bool_expr(b),
        NodeEnum::NullTest(n) => compile_null_test(n),
        other => Err(SqlError::Unsupported(format!("expression form: {other:?}"))),
    }
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
            let lhs = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SqlError::Internal("AExpr missing lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SqlError::Internal("AExpr missing rhs".into()))?;
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
                other => return Err(SqlError::Unsupported(format!("operator `{other}`"))),
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
                .ok_or_else(|| SqlError::Internal("BETWEEN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SqlError::Internal("BETWEEN without rhs".into()))?;
            let bounds = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) if l.items.len() == 2 => l.items.clone(),
                _ => return Err(SqlError::Internal("BETWEEN expects 2 bounds".into())),
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
        AExprKind::AexprIn => {
            let expr = a
                .lexpr
                .as_ref()
                .ok_or_else(|| SqlError::Internal("IN without lhs".into()))?;
            let rhs = a
                .rexpr
                .as_ref()
                .ok_or_else(|| SqlError::Internal("IN without rhs".into()))?;
            let items = match rhs.node.as_ref() {
                Some(NodeEnum::List(l)) => l.items.clone(),
                _ => return Err(SqlError::Internal("IN expects list".into())),
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
        other => Err(SqlError::Unsupported(format!("AExpr kind: {other:?}"))),
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
                .ok_or_else(|| SqlError::Internal("NOT without operand".into()))?;
            Ok(Expr::Not(Box::new(arg)))
        }
        _ => Err(SqlError::Unsupported(format!("BoolExpr {kind:?}"))),
    }
}

fn compile_null_test(n: &pg_query::protobuf::NullTest) -> Result<Expr> {
    use pg_query::protobuf::NullTestType;
    let arg = n
        .arg
        .as_ref()
        .ok_or_else(|| SqlError::Internal("NullTest without arg".into()))?;
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
        Val::Fval(f) => Value::Float(
            f.fval
                .parse::<f64>()
                .map_err(|e| SqlError::Internal(e.to_string()))?,
        ),
        Val::Sval(s) => Value::Str(s.sval.clone()),
        Val::Boolval(b) => Value::Bool(b.boolval),
        other => {
            return Err(SqlError::Unsupported(format!("constant: {other:?}")));
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
        0 => Err(SqlError::Internal("empty ColumnRef".into())),
        1 => Ok(Expr::Column(parts.pop().unwrap())),
        _ => {
            // `schema.table.col` collapses to `table.col`; `t.col`
            // round-trips as a qualified ref.
            let column = parts.pop().unwrap();
            let qualifier = parts.pop().unwrap();
            Ok(Expr::QualifiedColumn { qualifier, column })
        }
    }
}

fn compile_func_call(f: &pg_query::protobuf::FuncCall) -> Result<Expr> {
    let name = f
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
    let args = f
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    if let Some(over) = f.over.as_ref() {
        let spec = compile_window_spec(over)?;
        return Ok(Expr::WindowCall { name, args, spec });
    }
    Ok(Expr::Func { name, args })
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
                .ok_or_else(|| SqlError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            order_by.push(OrderBy { expr, descending });
        }
    }
    Ok(WindowSpec {
        partition_by,
        order_by,
    })
}

fn compile_type_cast(tc: &pg_query::protobuf::TypeCast) -> Result<Expr> {
    // Phase 5 accepts the cast but only carries forward the underlying
    // value; type-aware coercion lands when the type system widens.
    let arg = tc
        .arg
        .as_ref()
        .ok_or_else(|| SqlError::Internal("TypeCast without arg".into()))?;
    compile_expr(arg)
}

/// Convenience for tests that only need to round-trip through the
/// compiler without an Engine in scope.
pub fn plan_only_for_test(sql: &str) -> Result<Vec<Statement>> {
    compile(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(sql: &str) -> Statement {
        let mut v = compile(sql).unwrap();
        assert_eq!(v.len(), 1, "expected 1 stmt");
        v.remove(0)
    }

    #[test]
    fn create_table_with_vector_column() {
        let stmt =
            first("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, embedding VECTOR(4))");
        let Statement::CreateTable(ct) = stmt else {
            panic!("not CREATE TABLE");
        };
        assert_eq!(ct.name, "docs");
        assert_eq!(ct.columns.len(), 3);
        assert!(matches!(ct.columns[0].ty, ColumnType::Integer));
        assert!(ct.columns[0].primary_key);
        assert!(matches!(ct.columns[1].ty, ColumnType::Text));
        assert!(matches!(ct.columns[2].ty, ColumnType::Vector(4)));
    }

    #[test]
    fn create_index_records_access_method() {
        let stmt = first("CREATE INDEX idx_body ON docs USING gin (body)");
        let Statement::CreateIndex(ci) = stmt else {
            panic!("not CREATE INDEX");
        };
        assert_eq!(ci.table, "docs");
        assert_eq!(ci.access_method, "gin");
        assert_eq!(ci.columns, vec!["body"]);
    }

    #[test]
    fn insert_with_array_literal() {
        let stmt = first(
            "INSERT INTO docs (id, title, embedding) VALUES \
             (1, 'rust language', ARRAY[0.1, 0.2, 0.3])",
        );
        let Statement::Insert(i) = stmt else {
            panic!("not INSERT");
        };
        assert_eq!(i.table, "docs");
        assert_eq!(i.columns, vec!["id", "title", "embedding"]);
        assert_eq!(i.rows.len(), 1);
        assert_eq!(i.rows[0].len(), 3);
        match &i.rows[0][2] {
            Expr::Array(v) => assert_eq!(v.len(), 3),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn select_with_function_call_and_order_by() {
        let stmt = first(
            "SELECT id, title, _score AS s FROM docs \
             WHERE text_match(body, 'rust language') \
             ORDER BY _score DESC LIMIT 5",
        );
        let Statement::Select(s) = stmt else {
            panic!("not SELECT");
        };
        assert_eq!(s.projections.len(), 3);
        assert_eq!(s.projections[2].alias.as_deref(), Some("s"));
        match &s.from {
            Some(FromClause::Table { name, .. }) => assert_eq!(name, "docs"),
            other => panic!("expected single-table FROM, got {other:?}"),
        }
        assert!(matches!(s.r#where, Some(Expr::Func { .. })));
        assert_eq!(s.order_by.len(), 1);
        assert!(s.order_by[0].descending);
        assert_eq!(s.limit, Some(5));
    }
}
