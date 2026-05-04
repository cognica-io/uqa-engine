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
    ColumnDef, ColumnType, CreateIndex, CreateTable, Expr, InsertStmt, OrderBy, Projection,
    SelectStmt, Statement,
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
        NodeEnum::SelectStmt(stmt) => compile_select(stmt).map(Statement::Select),
        other => Err(SqlError::Unsupported(format!(
            "{}",
            other_node_label(other)
        ))),
    }
}

fn other_node_label(node: &NodeEnum) -> &'static str {
    match node {
        NodeEnum::UpdateStmt(_) => "UPDATE",
        NodeEnum::DeleteStmt(_) => "DELETE",
        NodeEnum::DropStmt(_) => "DROP",
        NodeEnum::ExplainStmt(_) => "EXPLAIN",
        NodeEnum::ViewStmt(_) => "CREATE VIEW",
        NodeEnum::TransactionStmt(_) => "BEGIN/COMMIT/ROLLBACK",
        NodeEnum::PrepareStmt(_) | NodeEnum::ExecuteStmt(_) => "PREPARE/EXECUTE",
        _ => "unknown statement",
    }
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
    Ok(CreateTable { name, columns })
}

fn compile_column_def(col: &pg_query::protobuf::ColumnDef) -> Result<ColumnDef> {
    let name = col.colname.clone();
    let ty = compile_type_name(col)?;
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
    Ok(ColumnDef {
        name,
        ty,
        primary_key,
        not_null,
    })
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
        "int" | "int4" | "integer" | "bigint" | "int8" | "smallint" | "int2" => {
            Ok(ColumnType::Integer)
        }
        "text" | "varchar" | "character" | "char" | "bpchar" => Ok(ColumnType::Text),
        "real" | "float4" | "float8" | "double" | "numeric" | "decimal" => Ok(ColumnType::Real),
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
    Ok(CreateIndex {
        name,
        table,
        access_method,
        columns,
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

fn compile_select(stmt: &pg_query::protobuf::SelectStmt) -> Result<SelectStmt> {
    let from = stmt
        .from_clause
        .first()
        .and_then(|n| n.node.as_ref())
        .and_then(|inner| match inner {
            NodeEnum::RangeVar(r) => Some(r.relname.clone()),
            _ => None,
        });

    let mut projections = Vec::new();
    for target_node in &stmt.target_list {
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
        projections.push(Projection { expr, alias });
    }

    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;

    let mut order_by = Vec::new();
    for sort_node in &stmt.sort_clause {
        let Some(inner) = sort_node.node.as_ref() else {
            continue;
        };
        if let NodeEnum::SortBy(sb) = inner {
            let expr_node = sb
                .node
                .as_ref()
                .ok_or_else(|| SqlError::Internal("SortBy without expr".into()))?;
            let expr = compile_expr(expr_node)?;
            // SortByDir enum: SortbyDefault = 0, SortbyAsc = 2,
            // SortbyDesc = 3, SortbyUsing = 4 (per libpg_query 6.x).
            let descending = sb.sortby_dir == pg_query::protobuf::SortByDir::SortbyDesc as i32;
            order_by.push(OrderBy { expr, descending });
        }
    }

    let limit = compile_int_const(stmt.limit_count.as_deref())?;
    let offset = compile_int_const(stmt.limit_offset.as_deref())?;

    Ok(SelectStmt {
        projections,
        from,
        r#where,
        order_by,
        limit,
        offset,
    })
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
        other => Err(SqlError::Unsupported(format!("expression form: {other:?}"))),
    }
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
    if parts.is_empty() {
        return Err(SqlError::Internal("empty ColumnRef".into()));
    }
    // Phase 5 ignores qualifying table names (e.g. `t.col` -> `col`).
    Ok(Expr::Column(parts.last().cloned().unwrap()))
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
    Ok(Expr::Func { name, args })
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
        assert_eq!(s.from.as_deref(), Some("docs"));
        assert!(matches!(s.r#where, Some(Expr::Func { .. })));
        assert_eq!(s.order_by.len(), 1);
        assert!(s.order_by[0].descending);
        assert_eq!(s.limit, Some(5));
    }
}
