//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTAS, prepared statements, foreign relations, views, and schemas.

use super::dispatch::compile_stmt;
use super::{
    compile_column_def, compile_expr, compile_select, extract_string, range_var_name,
    validate_create_table_envelope, validate_durable_create_relation, ColumnDef, Expr, Node,
    NodeEnum, Result, SQLError, Statement,
};

struct IntoTarget {
    name: String,
    column_names: Vec<String>,
    skip_data: bool,
}

fn compile_into_target(into: &pg_query::protobuf::IntoClause, command: &str) -> Result<IntoTarget> {
    use pg_query::protobuf::OnCommitAction;

    let relation = into
        .rel
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{command} target has no name")))?;
    validate_durable_create_relation(relation, command)?;
    let column_names = into
        .col_names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if !into.access_method.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{command} USING access methods are not supported"
        )));
    }
    if !into.options.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{command} storage options are not supported"
        )));
    }
    if !matches!(
        into.on_commit(),
        OnCommitAction::Undefined | OnCommitAction::OncommitNoop
    ) {
        return Err(SQLError::Unsupported(format!(
            "{command} ON COMMIT is not supported"
        )));
    }
    if !into.table_space_name.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{command} TABLESPACE is not supported"
        )));
    }
    if into.view_query.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{command} view-query payloads are not supported"
        )));
    }
    Ok(IntoTarget {
        name: range_var_name(relation),
        column_names,
        skip_data: into.skip_data,
    })
}

fn take_select_into_clause(
    stmt: &mut pg_query::protobuf::SelectStmt,
) -> Option<Box<pg_query::protobuf::IntoClause>> {
    stmt.into_clause
        .take()
        .or_else(|| stmt.larg.as_deref_mut().and_then(take_select_into_clause))
}

fn select_has_into_clause(stmt: &pg_query::protobuf::SelectStmt) -> bool {
    stmt.into_clause.is_some() || stmt.larg.as_deref().is_some_and(select_has_into_clause)
}

pub(super) fn compile_top_level_select(stmt: &pg_query::protobuf::SelectStmt) -> Result<Statement> {
    if !select_has_into_clause(stmt) {
        return compile_select(stmt).map(|select| Statement::Select(Box::new(select)));
    }
    let mut body = stmt.clone();
    let into = take_select_into_clause(&mut body)
        .ok_or_else(|| SQLError::Internal("SELECT INTO target disappeared".into()))?;
    let target = compile_into_target(&into, "SELECT INTO")?;
    if target.skip_data {
        return Err(SQLError::Internal(
            "SELECT INTO unexpectedly requested WITH NO DATA".into(),
        ));
    }
    Ok(Statement::CreateTableAs {
        name: target.name,
        if_not_exists: false,
        column_names: target.column_names,
        with_no_data: false,
        body: Box::new(compile_select(&body)?),
    })
}

pub(super) fn compile_create_table_as(
    stmt: &pg_query::protobuf::CreateTableAsStmt,
) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;

    match stmt.objtype() {
        ObjectType::ObjectTable => {}
        ObjectType::ObjectMatview => {
            return Err(SQLError::Unsupported(
                "CREATE MATERIALIZED VIEW is not supported".into(),
            ));
        }
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE TABLE AS object type {other:?} is not supported"
            )));
        }
    }
    let into = stmt
        .into
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without target".into()))?;
    let command = if stmt.is_select_into {
        "SELECT INTO"
    } else {
        "CREATE TABLE AS"
    };
    let target = compile_into_target(into, command)?;
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without body".into()))?;
    let inner = body
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS body empty".into()))?;
    let select = match inner {
        NodeEnum::SelectStmt(s) => compile_select(s)?,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE TABLE AS body must be SELECT, got {other:?}"
            )));
        }
    };
    Ok(Statement::CreateTableAs {
        name: target.name,
        if_not_exists: stmt.if_not_exists,
        column_names: target.column_names,
        with_no_data: target.skip_data,
        body: Box::new(select),
    })
}

pub(super) fn compile_prepare(stmt: &pg_query::protobuf::PrepareStmt) -> Result<Statement> {
    let name = stmt.name.clone();
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("PREPARE without body".into()))?;
    let inner = compile_stmt(body)?;
    Ok(Statement::Prepare {
        name,
        body: Box::new(inner),
    })
}

pub(super) fn compile_execute(stmt: &pg_query::protobuf::ExecuteStmt) -> Result<Statement> {
    let name = stmt.name.clone();
    let mut params: Vec<Expr> = Vec::with_capacity(stmt.params.len());
    for p in &stmt.params {
        params.push(compile_expr(p)?);
    }
    Ok(Statement::Execute { name, params })
}

pub(super) fn compile_deallocate(stmt: &pg_query::protobuf::DeallocateStmt) -> Result<Statement> {
    let name = if stmt.name.is_empty() {
        None
    } else {
        Some(stmt.name.clone())
    };
    Ok(Statement::Deallocate { name })
}

pub(super) fn compile_create_foreign_server(
    stmt: &pg_query::protobuf::CreateForeignServerStmt,
) -> Result<crate::ast::CreateForeignServer> {
    use crate::ast::CreateForeignServer;
    Ok(CreateForeignServer {
        name: stmt.servername.clone(),
        fdw_type: stmt.fdwname.clone(),
        options: collect_def_elem_options(&stmt.options)?,
        if_not_exists: stmt.if_not_exists,
    })
}

pub(super) fn compile_create_foreign_table(
    stmt: &pg_query::protobuf::CreateForeignTableStmt,
) -> Result<crate::ast::CreateForeignTable> {
    use crate::ast::CreateForeignTable;
    let base = stmt
        .base_stmt
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without base".into()))?;
    validate_create_table_envelope(base, "CREATE FOREIGN TABLE")?;
    let name = base
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("CREATE FOREIGN TABLE without name".into()))?;
    let mut columns: Vec<ColumnDef> = Vec::new();
    for elt in &base.table_elts {
        let Some(NodeEnum::ColumnDef(col)) = elt.node.as_ref() else {
            return Err(SQLError::Unsupported(
                "CREATE FOREIGN TABLE supports column definitions only".into(),
            ));
        };
        columns.push(compile_column_def(col)?);
    }
    Ok(CreateForeignTable {
        name,
        server_name: stmt.servername.clone(),
        columns,
        options: collect_def_elem_options(&stmt.options)?,
        if_not_exists: base.if_not_exists,
    })
}

pub(super) fn collect_def_elem_options(nodes: &[Node]) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for opt in nodes {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal("malformed option node".into()));
        };
        let value = match elem
            .arg
            .as_ref()
            .and_then(|argument| argument.node.as_ref())
        {
            Some(NodeEnum::String(value)) => value.sval.clone(),
            Some(NodeEnum::Integer(value)) => value.ival.to_string(),
            Some(NodeEnum::Float(value)) => value.fval.clone(),
            Some(NodeEnum::Boolean(value)) => value.boolval.to_string(),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "option `{}` expects a scalar value, got {other:?}",
                    elem.defname
                )));
            }
        };
        out.push((elem.defname.clone(), value));
    }
    Ok(out)
}

pub(super) fn compile_create_view(stmt: &pg_query::protobuf::ViewStmt) -> Result<Statement> {
    use pg_query::protobuf::ViewCheckOption;

    let relation = stmt
        .view
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW without name".into()))?;
    validate_durable_create_relation(relation, "CREATE VIEW")?;
    let column_names = stmt
        .aliases
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if !stmt.options.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE VIEW options are not supported".into(),
        ));
    }
    if !matches!(
        stmt.with_check_option(),
        ViewCheckOption::Undefined | ViewCheckOption::NoCheckOption
    ) {
        return Err(SQLError::Unsupported(
            "CREATE VIEW WITH CHECK OPTION is not supported".into(),
        ));
    }
    let name = range_var_name(relation);
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW without body".into()))?;
    let inner = body
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW body empty".into()))?;
    let select = match inner {
        NodeEnum::SelectStmt(s) => compile_select(s)?,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE VIEW body must be SELECT, got {other:?}"
            )));
        }
    };
    Ok(Statement::CreateView {
        name,
        column_names,
        body: Box::new(select),
        or_replace: stmt.replace,
    })
}

pub(super) fn compile_create_schema(
    stmt: &pg_query::protobuf::CreateSchemaStmt,
) -> Result<Statement> {
    if stmt.authrole.is_some() {
        return Err(SQLError::Unsupported(
            "CREATE SCHEMA AUTHORIZATION is not supported".into(),
        ));
    }
    if !stmt.schema_elts.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE SCHEMA containing schema elements is not supported".into(),
        ));
    }
    let name = if stmt.schemaname.is_empty() {
        return Err(SQLError::Internal("CREATE SCHEMA without name".into()));
    } else {
        stmt.schemaname.clone()
    };
    Ok(Statement::CreateSchema {
        name,
        if_not_exists: stmt.if_not_exists,
    })
}
