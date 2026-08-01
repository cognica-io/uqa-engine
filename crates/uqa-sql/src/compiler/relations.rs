//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTAS, prepared statements, foreign relations, views, and schemas.

use super::dispatch::compile_stmt;
use super::{
    compile_column_def, compile_expr, compile_select, range_var_name,
    validate_create_table_envelope, validate_durable_create_relation, ColumnDef, Expr, Node,
    NodeEnum, Result, SQLError, Statement,
};

pub(super) fn compile_create_table_as(
    stmt: &pg_query::protobuf::CreateTableAsStmt,
) -> Result<Statement> {
    use pg_query::protobuf::{ObjectType, OnCommitAction};

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
    if stmt.is_select_into {
        return Err(SQLError::Unsupported("SELECT INTO is not supported".into()));
    }
    let into = stmt
        .into
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without target".into()))?;
    let relation = into
        .rel
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS target has no name".into()))?;
    validate_durable_create_relation(relation, "CREATE TABLE AS")?;
    if !into.col_names.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS column-name lists are not supported".into(),
        ));
    }
    if !into.access_method.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS USING access methods are not supported".into(),
        ));
    }
    if !into.options.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS storage options are not supported".into(),
        ));
    }
    if !matches!(
        into.on_commit(),
        OnCommitAction::Undefined | OnCommitAction::OncommitNoop
    ) {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS ON COMMIT is not supported".into(),
        ));
    }
    if !into.table_space_name.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS TABLESPACE is not supported".into(),
        ));
    }
    if into.view_query.is_some() {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS view-query payloads are not supported".into(),
        ));
    }
    if into.skip_data {
        return Err(SQLError::Unsupported(
            "CREATE TABLE AS WITH NO DATA is not supported".into(),
        ));
    }
    let name = range_var_name(relation);
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
        name,
        if_not_exists: stmt.if_not_exists,
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
    if !stmt.aliases.is_empty() {
        return Err(SQLError::Unsupported(
            "CREATE VIEW column aliases are not supported".into(),
        ));
    }
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
