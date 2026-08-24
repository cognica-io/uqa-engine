//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CTAS, prepared statements, foreign relations, views, and schemas.

use super::dispatch::compile_stmt;
use super::{
    compile_column_def, compile_expr, compile_on_commit, compile_select, extract_string,
    range_var_name, relation_persistence, validate_create_table_envelope, ColumnDef, Expr, Node,
    NodeEnum, Result, SQLError, Statement,
};
use crate::ast::{OnCommitAction, RelationPersistence};

struct IntoTarget {
    name: String,
    column_names: Vec<String>,
    skip_data: bool,
    persistence: RelationPersistence,
    on_commit: OnCommitAction,
    options: Vec<(String, String)>,
}

fn compile_into_target(into: &pg_query::protobuf::IntoClause, command: &str) -> Result<IntoTarget> {
    let relation = into
        .rel
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{command} target has no name")))?;
    let persistence = relation_persistence(relation, command)?;
    let on_commit = compile_on_commit(into.on_commit(), persistence, command)?;
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
    let options = collect_def_elem_options(&into.options)?;
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
        persistence,
        on_commit,
        options,
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
        persistence: target.persistence,
        on_commit: target.on_commit,
        body: Box::new(compile_select(&body)?),
    })
}

pub(super) fn compile_create_table_as(
    stmt: &pg_query::protobuf::CreateTableAsStmt,
) -> Result<Statement> {
    use pg_query::protobuf::ObjectType;

    let materialized = match stmt.objtype() {
        ObjectType::ObjectTable => false,
        ObjectType::ObjectMatview => true,
        other => {
            return Err(SQLError::Unsupported(format!(
                "CREATE TABLE AS object type {other:?} is not supported"
            )));
        }
    };
    let into = stmt
        .into
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE TABLE AS without target".into()))?;
    let command = if materialized {
        "CREATE MATERIALIZED VIEW"
    } else if stmt.is_select_into {
        "SELECT INTO"
    } else {
        "CREATE TABLE AS"
    };
    let target = compile_into_target(into, command)?;
    if materialized {
        if target.persistence != RelationPersistence::Permanent {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "materialized views cannot be temporary or unlogged".into(),
            });
        }
        if target.on_commit != OnCommitAction::PreserveRows {
            return Err(SQLError::Routine {
                sqlstate: "42P16".into(),
                message: "ON COMMIT cannot be used on materialized views".into(),
            });
        }
    }
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
    if materialized {
        Ok(Statement::CreateMaterializedView {
            name: target.name,
            if_not_exists: stmt.if_not_exists,
            column_names: target.column_names,
            with_no_data: target.skip_data,
            options: validate_materialized_view_options(target.options)?,
            body: Box::new(select),
        })
    } else {
        if !target.options.is_empty() {
            return Err(SQLError::Unsupported(
                "CREATE TABLE AS storage options are not supported".into(),
            ));
        }
        Ok(Statement::CreateTableAs {
            name: target.name,
            if_not_exists: stmt.if_not_exists,
            column_names: target.column_names,
            with_no_data: target.skip_data,
            persistence: target.persistence,
            on_commit: target.on_commit,
            body: Box::new(select),
        })
    }
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
            Some(NodeEnum::TypeName(value)) => value
                .names
                .iter()
                .map(extract_string)
                .collect::<Result<Vec<_>>>()?
                .join("."),
            None => "true".into(),
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

fn invalid_reloption(message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: message.into(),
    }
}

fn validate_boolean_reloption(name: &str, value: &str) -> Result<()> {
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "false" | "on" | "off" | "yes" | "no" | "1" | "0"
    ) {
        Ok(())
    } else {
        Err(invalid_reloption(format!(
            "invalid value for boolean option \"{name}\": {value}"
        )))
    }
}

pub(super) fn validate_view_options(
    options: Vec<(String, String)>,
    check_option: pg_query::protobuf::ViewCheckOption,
) -> Result<Vec<(String, String)>> {
    use pg_query::protobuf::ViewCheckOption;
    use std::collections::BTreeSet;

    let mut out = Vec::with_capacity(options.len() + 1);
    let mut seen = BTreeSet::new();
    for (name, value) in options {
        let name = name.to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        if !seen.insert(name.clone()) {
            return Err(invalid_reloption(format!(
                "parameter \"{name}\" specified more than once"
            )));
        }
        match name.as_str() {
            "security_barrier" | "security_invoker" => {
                validate_boolean_reloption(&name, &value)?;
            }
            "check_option" if matches!(value.as_str(), "local" | "cascaded") => {}
            "check_option" => {
                return Err(invalid_reloption(format!(
                    "invalid value for enum option \"check_option\": {value}"
                )));
            }
            _ => {
                return Err(invalid_reloption(format!(
                    "unrecognized parameter \"{name}\""
                )));
            }
        }
        out.push((name, value));
    }
    let clause_value = match check_option {
        ViewCheckOption::Undefined | ViewCheckOption::NoCheckOption => None,
        ViewCheckOption::LocalCheckOption => Some("local"),
        ViewCheckOption::CascadedCheckOption => Some("cascaded"),
    };
    if let Some(value) = clause_value {
        if !seen.insert("check_option".into()) {
            return Err(invalid_reloption(
                "parameter \"check_option\" specified more than once",
            ));
        }
        out.push(("check_option".into(), value.into()));
    }
    Ok(out)
}

pub(super) fn validate_materialized_view_options(
    options: Vec<(String, String)>,
) -> Result<Vec<(String, String)>> {
    use std::collections::BTreeSet;

    let mut seen = BTreeSet::new();
    for (name, value) in &options {
        let name = name.to_ascii_lowercase();
        if !seen.insert(name.clone()) {
            return Err(invalid_reloption(format!(
                "parameter \"{name}\" specified more than once"
            )));
        }
        if name != "fillfactor" {
            return Err(invalid_reloption(format!(
                "unrecognized parameter \"{name}\""
            )));
        }
        let fillfactor = value.parse::<u8>().map_err(|_| {
            invalid_reloption(format!(
                "invalid value for integer option \"fillfactor\": {value}"
            ))
        })?;
        if !(10..=100).contains(&fillfactor) {
            return Err(invalid_reloption(format!(
                "value {fillfactor} out of bounds for option \"fillfactor\""
            )));
        }
    }
    Ok(options
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.to_ascii_lowercase()))
        .collect())
}

pub(super) fn compile_create_view(stmt: &pg_query::protobuf::ViewStmt) -> Result<Statement> {
    let relation = stmt
        .view
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE VIEW without name".into()))?;
    let persistence = relation_persistence(relation, "CREATE VIEW")?;
    if persistence == RelationPersistence::Unlogged {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "views cannot be unlogged because they do not have storage".into(),
        });
    }
    let column_names = stmt
        .aliases
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    let options = validate_view_options(
        collect_def_elem_options(&stmt.options)?,
        stmt.with_check_option(),
    )?;
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
        persistence,
        options,
    })
}

pub(super) fn compile_refresh_materialized_view(
    stmt: &pg_query::protobuf::RefreshMatViewStmt,
) -> Result<Statement> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal("REFRESH MATERIALIZED VIEW without name".into()))?;
    if !relation.catalogname.is_empty() {
        return Err(SQLError::Unsupported(
            "REFRESH MATERIALIZED VIEW: cross-database names are not supported".into(),
        ));
    }
    Ok(Statement::RefreshMaterializedView {
        name: range_var_name(relation),
        concurrently: stmt.concurrent,
        with_no_data: stmt.skip_data,
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
