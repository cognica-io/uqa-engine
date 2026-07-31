//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift a `pg_query` parse tree into the internal [`Statement`] AST.
//!
//! The compiler accepts `PostgreSQL` syntax through `pg_query` and lifts
//! supported statements into the internal AST. Syntax that parses but is
//! outside the current SQL surface compiles to [`SQLError::Unsupported`].

use crate::ast::{
    AlterTableAction, AlterTableStmt, ColumnDef, DeleteStmt, DropKind, DropStmt, Expr, Statement,
    TableKeyConstraint, TableKeyConstraintKind, TransactionStmt, UpdateStmt,
};
use crate::error::{Result, SQLError};
use pg_query::protobuf::{Node, RangeVar};
use pg_query::NodeEnum;
use types::compile_pg_type_name;

mod tree;
mod types;

use tree::{
    compile_column_def, compile_create_index, compile_create_table, compile_expr,
    compile_from_node, compile_insert, compile_projections, compile_select, compile_with_clause,
    extract_string,
};

pub(super) fn range_var_name(r: &RangeVar) -> String {
    if r.schemaname.is_empty() {
        render_relation_component(&r.relname)
    } else {
        format!(
            "{}.{}",
            render_relation_component(&r.schemaname),
            render_relation_component(&r.relname)
        )
    }
}

/// Reject relation qualifiers that the durable catalog cannot faithfully
/// represent.  `PostgreSQL` records temporary/unlogged persistence separately;
/// silently storing either as a permanent relation would change restart
/// semantics.
pub(super) fn validate_durable_create_relation(relation: &RangeVar, statement: &str) -> Result<()> {
    if !relation.catalogname.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: cross-database relation names are not supported"
        )));
    }
    match relation.relpersistence.as_str() {
        "" | "p" => Ok(()),
        "t" => Err(SQLError::Unsupported(format!(
            "{statement}: TEMPORARY relations are not supported"
        ))),
        "u" => Err(SQLError::Unsupported(format!(
            "{statement}: UNLOGGED relations are not supported"
        ))),
        other => Err(SQLError::Unsupported(format!(
            "{statement}: relation persistence `{other}` is not supported"
        ))),
    }
}

pub(super) fn validate_create_table_envelope(
    stmt: &pg_query::protobuf::CreateStmt,
    statement: &str,
) -> Result<()> {
    use pg_query::protobuf::OnCommitAction;

    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{statement} without relation")))?;
    validate_durable_create_relation(relation, statement)?;
    if !stmt.inh_relations.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: INHERITS is not supported"
        )));
    }
    if stmt.partbound.is_some() || stmt.partspec.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: partitioning is not supported"
        )));
    }
    if stmt.of_typename.is_some() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: typed tables are not supported"
        )));
    }
    if !stmt.constraints.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: out-of-line constraint payloads are not supported"
        )));
    }
    if !stmt.options.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: table storage options are not supported"
        )));
    }
    if !matches!(
        stmt.oncommit(),
        OnCommitAction::Undefined | OnCommitAction::OncommitNoop
    ) {
        return Err(SQLError::Unsupported(format!(
            "{statement}: ON COMMIT is not supported"
        )));
    }
    if !stmt.tablespacename.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: TABLESPACE is not supported"
        )));
    }
    if !stmt.access_method.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: USING access methods are not supported"
        )));
    }
    Ok(())
}

fn render_relation_component(component: &str) -> String {
    let can_render_bare = component
        .bytes()
        .enumerate()
        .all(|(index, byte)| match byte {
            b'a'..=b'z' | b'_' => true,
            b'0'..=b'9' | b'$' => index != 0,
            _ => false,
        });
    if can_render_bare && !component.is_empty() {
        component.to_string()
    } else {
        format!("\"{}\"", component.replace('"', "\"\""))
    }
}

pub(super) fn compile_qualified_name(parts: &[Node], statement: &str) -> Result<String> {
    let parts = parts
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?;
    if parts.is_empty() {
        return Err(SQLError::Internal(format!(
            "{statement} without a routine name"
        )));
    }
    if parts.len() > 2 {
        return Err(SQLError::Unsupported(format!(
            "{statement}: cross-database routine names are not supported"
        )));
    }
    Ok(parts
        .iter()
        .map(|part| render_relation_component(part))
        .collect::<Vec<_>>()
        .join("."))
}

pub fn compile(sql: &str) -> Result<Vec<Statement>> {
    let parsed = pg_query::parse(sql)?;
    let mut out = Vec::with_capacity(parsed.protobuf.stmts.len());
    for raw in parsed.protobuf.stmts {
        let node = raw
            .stmt
            .ok_or_else(|| SQLError::Internal("parser returned an empty statement".into()))?;
        out.push(compile_stmt(&node)?);
    }
    Ok(out)
}

fn compile_stmt(node: &Node) -> Result<Statement> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Unsupported("empty statement".into()));
    };
    match inner {
        NodeEnum::CreateStmt(stmt) => compile_create_table(stmt).map(Statement::CreateTable),
        NodeEnum::IndexStmt(stmt) => compile_create_index(stmt).map(Statement::CreateIndex),
        NodeEnum::InsertStmt(stmt) => compile_insert(stmt).map(Statement::Insert),
        NodeEnum::SelectStmt(stmt) => {
            // Standalone `VALUES (...) (...)` parses as a SelectStmt
            // with empty target_list + populated values_lists. Treat
            // it as a relation-producing statement directly.
            if stmt.target_list.is_empty() && !stmt.values_lists.is_empty() {
                let mut rows: Vec<Vec<Expr>> = Vec::new();
                for r in &stmt.values_lists {
                    let Some(NodeEnum::List(list)) = r.node.as_ref() else {
                        return Err(SQLError::Internal("VALUES contains a malformed row".into()));
                    };
                    let row: Vec<Expr> = list
                        .items
                        .iter()
                        .map(compile_expr)
                        .collect::<Result<Vec<_>>>()?;
                    rows.push(row);
                }
                return Ok(Statement::Values { rows });
            }
            compile_select(stmt).map(|s| Statement::Select(Box::new(s)))
        }
        NodeEnum::UpdateStmt(stmt) => compile_update(stmt).map(Statement::Update),
        NodeEnum::DeleteStmt(stmt) => compile_delete(stmt).map(Statement::Delete),
        NodeEnum::DropStmt(stmt) => compile_drop(stmt),
        NodeEnum::AlterTableStmt(stmt) => compile_alter_table(stmt).map(Statement::AlterTable),
        NodeEnum::RenameStmt(stmt) => compile_rename(stmt).map(Statement::AlterTable),
        NodeEnum::ViewStmt(stmt) => compile_create_view(stmt),
        NodeEnum::CreateSchemaStmt(stmt) => compile_create_schema(stmt),
        NodeEnum::ExplainStmt(stmt) => compile_explain(stmt),
        NodeEnum::VacuumStmt(stmt) => compile_analyze(stmt),
        NodeEnum::TruncateStmt(stmt) => compile_truncate(stmt),
        NodeEnum::TransactionStmt(stmt) => compile_transaction(stmt),
        NodeEnum::CreateSeqStmt(stmt) => {
            compile_create_sequence(stmt).map(Statement::CreateSequence)
        }
        NodeEnum::AlterSeqStmt(stmt) => compile_alter_sequence(stmt).map(Statement::AlterSequence),
        NodeEnum::CreateTableAsStmt(stmt) => compile_create_table_as(stmt),
        NodeEnum::PrepareStmt(stmt) => compile_prepare(stmt),
        NodeEnum::ExecuteStmt(stmt) => compile_execute(stmt),
        NodeEnum::DeallocateStmt(stmt) => compile_deallocate(stmt),
        NodeEnum::CreateForeignServerStmt(stmt) => {
            compile_create_foreign_server(stmt).map(Statement::CreateForeignServer)
        }
        NodeEnum::CreateForeignTableStmt(stmt) => {
            compile_create_foreign_table(stmt).map(Statement::CreateForeignTable)
        }
        NodeEnum::MergeStmt(stmt) => compile_merge(stmt).map(Statement::Merge),
        NodeEnum::CreateFunctionStmt(stmt) => {
            compile_create_function(stmt).map(|f| Statement::CreateFunction(Box::new(f)))
        }
        NodeEnum::DoStmt(stmt) => compile_do(stmt),
        NodeEnum::CallStmt(stmt) => compile_call(stmt),
        NodeEnum::VariableSetStmt(stmt) => compile_variable_set(stmt),
        NodeEnum::VariableShowStmt(stmt) => Ok(Statement::ShowVariable {
            name: stmt.name.clone(),
        }),
        NodeEnum::DiscardStmt(stmt) => Ok(Statement::Discard {
            target: discard_target(stmt.target)?,
        }),
        other => Err(SQLError::Unsupported(format!(
            "{}",
            other_node_label(other)
        ))),
    }
}

/// Map `pg_query`'s `DiscardMode` enum (1=ALL, 2=PLANS, 3=SEQUENCES,
/// 4=TEMP) to the AST's [`DiscardTarget`].
fn discard_target(mode: i32) -> Result<crate::ast::DiscardTarget> {
    use crate::ast::DiscardTarget;
    match mode {
        1 => Ok(DiscardTarget::All),
        2 => Ok(DiscardTarget::Plans),
        3 => Ok(DiscardTarget::Sequences),
        4 => Ok(DiscardTarget::Temp),
        other => Err(SQLError::Internal(format!(
            "unknown DISCARD target {other}"
        ))),
    }
}

fn compile_analyze(stmt: &pg_query::protobuf::VacuumStmt) -> Result<Statement> {
    if stmt.is_vacuumcmd {
        return Err(SQLError::Unsupported(
            "VACUUM is not implemented; VACUUM must not be treated as ANALYZE".into(),
        ));
    }

    if !stmt.options.is_empty() {
        return Err(SQLError::Unsupported(
            "ANALYZE options are not implemented".into(),
        ));
    }

    let table = match stmt.rels.as_slice() {
        [] => None,
        [node] => {
            let Some(NodeEnum::VacuumRelation(relation)) = node.node.as_ref() else {
                return Err(SQLError::Internal(
                    "ANALYZE contains a malformed relation".into(),
                ));
            };
            if relation.oid != 0 {
                return Err(SQLError::Unsupported(
                    "OID-targeted ANALYZE is not implemented".into(),
                ));
            }
            if !relation.va_cols.is_empty() {
                return Err(SQLError::Unsupported(
                    "ANALYZE column lists are not implemented".into(),
                ));
            }
            let range = relation.relation.as_ref().ok_or_else(|| {
                SQLError::Internal("ANALYZE relation is missing its table name".into())
            })?;
            if !range.catalogname.is_empty() {
                return Err(SQLError::Unsupported(
                    "cross-database ANALYZE is not implemented".into(),
                ));
            }
            if range.relname.is_empty() {
                return Err(SQLError::Internal(
                    "ANALYZE relation has an empty table name".into(),
                ));
            }
            Some(range_var_name(range))
        }
        _ => {
            return Err(SQLError::Unsupported(
                "ANALYZE of multiple tables is not implemented".into(),
            ));
        }
    };

    Ok(Statement::Analyze { table })
}

fn compile_variable_set(stmt: &pg_query::protobuf::VariableSetStmt) -> Result<Statement> {
    // Capture each argument as a string and join with commas. PG's
    // SET search_path TO a, b, c arrives as a list of A_Const nodes.
    let mut parts: Vec<String> = Vec::new();
    for arg in &stmt.args {
        let node = arg
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("SET contains an empty argument".into()))?;
        match node {
            NodeEnum::AConst(constant) => match constant.val.as_ref() {
                Some(pg_query::protobuf::a_const::Val::Sval(value)) => {
                    parts.push(value.sval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Ival(value)) => {
                    parts.push(value.ival.to_string());
                }
                Some(pg_query::protobuf::a_const::Val::Fval(value)) => {
                    parts.push(value.fval.clone());
                }
                Some(pg_query::protobuf::a_const::Val::Boolval(value)) => {
                    parts.push(value.boolval.to_string());
                }
                None if constant.isnull => parts.push("NULL".into()),
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "SET argument {other:?} is not supported"
                    )));
                }
            },
            NodeEnum::TypeCast(cast) => {
                let Some(NodeEnum::AConst(constant)) = cast
                    .arg
                    .as_ref()
                    .and_then(|argument| argument.node.as_ref())
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a literal".into(),
                    ));
                };
                let Some(pg_query::protobuf::a_const::Val::Sval(value)) = constant.val.as_ref()
                else {
                    return Err(SQLError::Unsupported(
                        "SET type-cast argument must contain a string literal".into(),
                    ));
                };
                parts.push(value.sval.clone());
            }
            NodeEnum::String(value) => parts.push(value.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "SET argument {other:?} is not supported"
                )));
            }
        }
    }
    let value = if stmt.name.eq_ignore_ascii_case("search_path") {
        parts
            .iter()
            .map(|part| render_relation_component(part))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        parts.join(",")
    };
    Ok(Statement::SetVariable {
        name: stmt.name.clone(),
        value,
    })
}

fn compile_create_sequence(
    stmt: &pg_query::protobuf::CreateSeqStmt,
) -> Result<crate::ast::CreateSequence> {
    use crate::ast::CreateSequence;
    let relation = stmt
        .sequence
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CREATE SEQUENCE without name".into()))?;
    validate_durable_create_relation(relation, "CREATE SEQUENCE")?;
    if stmt.owner_id != 0 || stmt.for_identity {
        return Err(SQLError::Unsupported(
            "CREATE SEQUENCE: identity-owned sequences are not supported".into(),
        ));
    }
    let name = range_var_name(relation);
    let mut start = None;
    let mut increment = 1_i64;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "CREATE SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        let value = compile_sequence_integer_option(elem, "CREATE SEQUENCE")?;
        match key.as_str() {
            "start" => start = Some(value),
            "increment" => increment = value,
            other => {
                return Err(SQLError::Unsupported(format!(
                    "CREATE SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    Ok(CreateSequence {
        name,
        if_not_exists: stmt.if_not_exists,
        // With the unsupported MINVALUE/MAXVALUE clauses excluded above,
        // the SQL defaults are 1 for ascending sequences and -1 for
        // descending sequences.
        start: start.unwrap_or(if increment > 0 { 1 } else { -1 }),
        increment,
    })
}

fn compile_alter_sequence(
    stmt: &pg_query::protobuf::AlterSeqStmt,
) -> Result<crate::ast::AlterSequence> {
    use crate::ast::{AlterSequence, SequenceRestart};
    if stmt.for_identity {
        return Err(SQLError::Unsupported(
            "ALTER SEQUENCE: identity-owned sequences are not supported".into(),
        ));
    }
    let name = stmt
        .sequence
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("ALTER SEQUENCE without name".into()))?;
    let mut alter = AlterSequence {
        name,
        if_exists: stmt.missing_ok,
        ..Default::default()
    };
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "ALTER SEQUENCE contains a malformed option".into(),
            ));
        };
        let key = elem.defname.to_ascii_lowercase();
        let value = if elem.arg.is_none() && key == "restart" {
            None
        } else {
            Some(compile_sequence_integer_option(elem, "ALTER SEQUENCE")?)
        };
        match key.as_str() {
            "restart" => {
                alter.restart = value.map_or(SequenceRestart::FromStart, SequenceRestart::With);
            }
            "increment" => alter.increment = value,
            "start" => alter.start = value,
            other => {
                return Err(SQLError::Unsupported(format!(
                    "ALTER SEQUENCE option `{other}` is not supported"
                )));
            }
        }
    }
    Ok(alter)
}

fn compile_sequence_integer_option(
    elem: &pg_query::protobuf::DefElem,
    statement: &str,
) -> Result<i64> {
    let raw = match elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    {
        Some(NodeEnum::Integer(value)) => return Ok(i64::from(value.ival)),
        Some(NodeEnum::Float(value)) => value.fval.as_str(),
        Some(NodeEnum::String(value)) => value.sval.as_str(),
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{statement} option `{}` expects an integer, got {other:?}",
                elem.defname
            )));
        }
    };
    raw.parse::<i64>().map_err(|_| {
        SQLError::TypeMismatch(format!(
            "{statement} option `{}` expects an integer, got `{raw}`",
            elem.defname
        ))
    })
}

fn compile_create_table_as(stmt: &pg_query::protobuf::CreateTableAsStmt) -> Result<Statement> {
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

fn compile_prepare(stmt: &pg_query::protobuf::PrepareStmt) -> Result<Statement> {
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

fn compile_execute(stmt: &pg_query::protobuf::ExecuteStmt) -> Result<Statement> {
    let name = stmt.name.clone();
    let mut params: Vec<Expr> = Vec::with_capacity(stmt.params.len());
    for p in &stmt.params {
        params.push(compile_expr(p)?);
    }
    Ok(Statement::Execute { name, params })
}

fn compile_deallocate(stmt: &pg_query::protobuf::DeallocateStmt) -> Result<Statement> {
    let name = if stmt.name.is_empty() {
        None
    } else {
        Some(stmt.name.clone())
    };
    Ok(Statement::Deallocate { name })
}

fn compile_create_foreign_server(
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

fn compile_create_foreign_table(
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

fn compile_merge(stmt: &pg_query::protobuf::MergeStmt) -> Result<crate::ast::MergeStmt> {
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

    let returning = compile_projections(&stmt.returning_list)?;
    Ok(MergeStmt {
        target,
        target_alias,
        source,
        join_condition,
        when_clauses,
        returning,
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

fn compile_create_view(stmt: &pg_query::protobuf::ViewStmt) -> Result<Statement> {
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

fn compile_create_schema(stmt: &pg_query::protobuf::CreateSchemaStmt) -> Result<Statement> {
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

/// Last non-`pg_catalog` segment of a `TypeName`, lower-cased, with
/// `%TYPE` and array-bound suffixes preserved so the executor can
/// treat them as uncastable (best-effort) types.
fn compile_function_type_name(t: &pg_query::protobuf::TypeName) -> Result<String> {
    let mut last = String::new();
    for n in &t.names {
        let name = extract_string(n)?;
        if name != "pg_catalog" {
            last = name;
        }
    }
    if last.is_empty() {
        return Err(SQLError::Internal(
            "function type has no name components".into(),
        ));
    }
    // `setof` is inspected separately by the caller; the name itself
    // stays scalar.
    let mut name = last.trim().to_ascii_lowercase();
    if t.pct_type {
        name.push_str("%type");
    }
    for _ in &t.array_bounds {
        name.push_str("[]");
    }
    Ok(name)
}

/// String payload of a `DefElem` argument.
fn def_elem_string(elem: &pg_query::protobuf::DefElem) -> Result<String> {
    match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
        Some(NodeEnum::String(s)) => Ok(s.sval.clone()),
        other => Err(SQLError::TypeMismatch(format!(
            "option `{}` expects a string, got {other:?}",
            elem.defname
        ))),
    }
}

fn compile_create_function(
    stmt: &pg_query::protobuf::CreateFunctionStmt,
) -> Result<crate::ast::CreateFunction> {
    use crate::ast::{
        CreateFunction, FunctionBody, FunctionParam, FunctionParamMode, FunctionReturns,
        FunctionVolatility,
    };
    use pg_query::protobuf::FunctionParameterMode;

    let keyword = if stmt.is_procedure {
        "CREATE PROCEDURE"
    } else {
        "CREATE FUNCTION"
    };
    let name = compile_qualified_name(&stmt.funcname, keyword)?;

    let mut params: Vec<FunctionParam> = Vec::with_capacity(stmt.parameters.len());
    let mut has_table_param = false;
    for p in &stmt.parameters {
        let Some(NodeEnum::FunctionParameter(fp)) = p.node.as_ref() else {
            return Err(SQLError::Internal(format!(
                "{keyword}: malformed parameter"
            )));
        };
        let mode = match fp.mode() {
            FunctionParameterMode::FuncParamIn | FunctionParameterMode::FuncParamDefault => {
                FunctionParamMode::In
            }
            FunctionParameterMode::FuncParamOut => FunctionParamMode::Out,
            FunctionParameterMode::FuncParamInout => FunctionParamMode::InOut,
            FunctionParameterMode::FuncParamTable => {
                has_table_param = true;
                FunctionParamMode::Table
            }
            FunctionParameterMode::FuncParamVariadic => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: VARIADIC parameters"
                )));
            }
            FunctionParameterMode::Undefined => {
                return Err(SQLError::Internal(format!(
                    "{keyword}: parameter mode missing"
                )));
            }
        };
        let type_name = fp
            .arg_type
            .as_ref()
            .map(compile_function_type_name)
            .transpose()?
            .ok_or_else(|| SQLError::Internal(format!("{keyword}: parameter without type")))?;
        let default = match fp.defexpr.as_ref() {
            Some(node) => Some(compile_expr(node)?),
            None => None,
        };
        params.push(FunctionParam {
            name: fp.name.to_ascii_lowercase(),
            type_name,
            mode,
            default,
        });
    }

    // Mirror PostgreSQL's parse-time rule: once an input parameter
    // has a DEFAULT, every following input parameter needs one too.
    let mut saw_default = false;
    for p in &params {
        if !matches!(p.mode, FunctionParamMode::In | FunctionParamMode::InOut) {
            continue;
        }
        if p.default.is_some() {
            saw_default = true;
        } else if saw_default {
            return Err(SQLError::Unsupported(
                "input parameters after one with a default value must also have defaults".into(),
            ));
        }
    }

    let returns = if has_table_param {
        FunctionReturns::Table
    } else {
        match stmt.return_type.as_ref() {
            None => FunctionReturns::None,
            Some(t) => {
                let type_name = compile_function_type_name(t)?;
                if t.setof {
                    FunctionReturns::SetOf { type_name }
                } else {
                    FunctionReturns::Scalar { type_name }
                }
            }
        }
    };

    let mut language = String::new();
    let mut volatility = FunctionVolatility::Volatile;
    let mut strict = false;
    let mut source: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(format!("{keyword}: malformed option")));
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "language" => {
                language = def_elem_string(elem)?.to_ascii_lowercase();
            }
            "volatility" => {
                volatility = match def_elem_string(elem)?.as_str() {
                    "immutable" => FunctionVolatility::Immutable,
                    "stable" => FunctionVolatility::Stable,
                    "volatile" => FunctionVolatility::Volatile,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: invalid volatility `{other}`"
                        )));
                    }
                };
            }
            "strict" => {
                strict = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::Boolean(value)) => value.boolval,
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: STRICT expects a boolean, got {other:?}"
                        )));
                    }
                };
            }
            "as" => {
                let items: Vec<String> = match elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    Some(NodeEnum::List(list)) => list
                        .items
                        .iter()
                        .map(extract_string)
                        .collect::<Result<Vec<_>>>()?,
                    Some(NodeEnum::String(s)) => vec![s.sval.clone()],
                    other => {
                        return Err(SQLError::TypeMismatch(format!(
                            "{keyword}: AS expects a string body, got {other:?}"
                        )));
                    }
                };
                match items.len() {
                    1 => source = items.into_iter().next(),
                    _ => {
                        return Err(SQLError::Unsupported(format!(
                            "{keyword}: AS 'obj_file', 'link_symbol' bodies"
                        )));
                    }
                }
            }
            "window" => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: WINDOW functions"
                )));
            }
            // Planner / execution hints without engine semantics:
            // COST, ROWS, PARALLEL, SECURITY, LEAKPROOF, SET, SUPPORT.
            other => {
                return Err(SQLError::Unsupported(format!(
                    "{keyword}: option `{other}` is not supported"
                )));
            }
        }
    }

    let body = match (source, stmt.sql_body.as_deref()) {
        (Some(src), None) => FunctionBody::Source(src),
        (None, Some(node)) => FunctionBody::Statements(compile_sql_standard_body(node)?),
        (Some(_), Some(_)) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: both AS body and SQL-standard body"
            )));
        }
        (None, None) => {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no function body"
            )));
        }
    };
    if language.is_empty() {
        if matches!(body, FunctionBody::Statements(_)) {
            language = "sql".into();
        } else {
            return Err(SQLError::Unsupported(format!(
                "{keyword}: no language specified"
            )));
        }
    }

    Ok(CreateFunction {
        name,
        or_replace: stmt.replace,
        is_procedure: stmt.is_procedure,
        params,
        returns,
        language,
        body,
        volatility,
        strict,
    })
}

/// Compile a SQL-standard function body (`RETURN expr` or
/// `BEGIN ATOMIC stmt; ... END`) into plain statements.
fn compile_sql_standard_body(node: &Node) -> Result<Vec<Statement>> {
    let Some(inner) = node.node.as_ref() else {
        return Err(SQLError::Internal("empty SQL function body".into()));
    };
    match inner {
        NodeEnum::ReturnStmt(ret) => {
            let value = ret
                .returnval
                .as_deref()
                .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
            Ok(vec![select_of_expr(compile_expr(value)?)])
        }
        NodeEnum::List(list) => {
            let mut out = Vec::with_capacity(list.items.len());
            for item in &list.items {
                let item_inner = item.node.as_ref().ok_or_else(|| {
                    SQLError::Internal("SQL function body contains an empty statement".into())
                })?;
                match item_inner {
                    // BEGIN ATOMIC wraps each statement in a nested list.
                    NodeEnum::List(stmts) => {
                        for s in &stmts.items {
                            out.push(compile_stmt(s)?);
                        }
                    }
                    NodeEnum::ReturnStmt(ret) => {
                        let value = ret
                            .returnval
                            .as_deref()
                            .ok_or_else(|| SQLError::Internal("RETURN without a value".into()))?;
                        out.push(select_of_expr(compile_expr(value)?));
                    }
                    _ => out.push(compile_stmt(item)?),
                }
            }
            Ok(out)
        }
        other => Err(SQLError::Unsupported(format!(
            "SQL function body node {other:?}"
        ))),
    }
}

/// `SELECT <expr>` statement wrapping a single expression.
fn select_of_expr(expr: Expr) -> Statement {
    Statement::Select(Box::new(crate::ast::SelectStmt {
        projections: vec![crate::ast::Projection { expr, alias: None }],
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        having: None,
        order_by: Vec::new(),
        limit: None,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
    }))
}

fn compile_do(stmt: &pg_query::protobuf::DoStmt) -> Result<Statement> {
    let mut language = "plpgsql".to_string();
    let mut body: Option<String> = None;
    for arg in &stmt.args {
        let Some(NodeEnum::DefElem(elem)) = arg.node.as_ref() else {
            return Err(SQLError::Internal("DO contains a malformed option".into()));
        };
        match elem.defname.to_ascii_lowercase().as_str() {
            "as" => body = Some(def_elem_string(elem)?),
            "language" => {
                language = def_elem_string(elem)?.to_ascii_lowercase();
            }
            other => {
                return Err(SQLError::Unsupported(format!(
                    "DO option `{other}` is not supported"
                )));
            }
        }
    }
    let body = body.ok_or_else(|| SQLError::Internal("DO without a body".into()))?;
    Ok(Statement::DoBlock { language, body })
}

fn compile_call(stmt: &pg_query::protobuf::CallStmt) -> Result<Statement> {
    let call = stmt
        .funccall
        .as_ref()
        .ok_or_else(|| SQLError::Internal("CALL without a function".into()))?;
    let name = compile_qualified_name(&call.funcname, "CALL")?;
    let args = call
        .args
        .iter()
        .map(compile_expr)
        .collect::<Result<Vec<_>>>()?;
    Ok(Statement::Call { name, args })
}

fn compile_explain(stmt: &pg_query::protobuf::ExplainStmt) -> Result<Statement> {
    let body = stmt
        .query
        .as_deref()
        .ok_or_else(|| SQLError::Internal("EXPLAIN without body".into()))?;
    let mut analyze = false;
    let mut verbose = false;
    let mut format: Option<String> = None;
    for opt in &stmt.options {
        let Some(NodeEnum::DefElem(elem)) = opt.node.as_ref() else {
            return Err(SQLError::Internal(
                "EXPLAIN contains a malformed option".into(),
            ));
        };
        let name = elem.defname.to_ascii_lowercase();
        match name.as_str() {
            "analyze" => analyze = compile_explain_bool_option(elem, "ANALYZE")?,
            "verbose" => verbose = compile_explain_bool_option(elem, "VERBOSE")?,
            "format" => {
                if let Some(NodeEnum::String(s)) = elem.arg.as_ref().and_then(|a| a.node.as_ref()) {
                    format = Some(s.sval.clone());
                } else {
                    return Err(SQLError::TypeMismatch(
                        "EXPLAIN FORMAT expects a format name".into(),
                    ));
                }
            }
            _ => {
                return Err(SQLError::Unsupported(format!(
                    "EXPLAIN option `{name}` is not supported"
                )));
            }
        }
    }
    let inner = compile_stmt(body)?;
    Ok(Statement::Explain {
        analyze,
        verbose,
        format,
        body: Box::new(inner),
    })
}

fn compile_explain_bool_option(elem: &pg_query::protobuf::DefElem, name: &str) -> Result<bool> {
    let Some(argument) = elem
        .arg
        .as_ref()
        .and_then(|argument| argument.node.as_ref())
    else {
        return Ok(true);
    };
    match argument {
        NodeEnum::Boolean(value) => Ok(value.boolval),
        NodeEnum::Integer(value) => Ok(value.ival != 0),
        NodeEnum::String(value) => match value.sval.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Ok(true),
            "false" | "off" | "no" | "0" => Ok(false),
            _ => Err(SQLError::TypeMismatch(format!(
                "EXPLAIN {name} expects a boolean value"
            ))),
        },
        _ => Err(SQLError::TypeMismatch(format!(
            "EXPLAIN {name} expects a boolean value"
        ))),
    }
}

fn compile_truncate(stmt: &pg_query::protobuf::TruncateStmt) -> Result<Statement> {
    let mut tables = Vec::new();
    for relation in &stmt.relations {
        let Some(NodeEnum::RangeVar(range)) = relation.node.as_ref() else {
            return Err(SQLError::Internal(
                "TRUNCATE contains a malformed table target".into(),
            ));
        };
        tables.push(range_var_name(range));
    }
    if tables.is_empty() {
        return Err(SQLError::Internal("TRUNCATE without a table".into()));
    }
    let cascade = matches!(
        stmt.behavior(),
        pg_query::protobuf::DropBehavior::DropCascade
    );
    Ok(Statement::Truncate { tables, cascade })
}

fn compile_transaction(stmt: &pg_query::protobuf::TransactionStmt) -> Result<Statement> {
    use pg_query::protobuf::TransactionStmtKind;
    let kind = match stmt.kind() {
        TransactionStmtKind::TransStmtBegin | TransactionStmtKind::TransStmtStart => {
            TransactionStmt::Begin
        }
        TransactionStmtKind::TransStmtCommit => TransactionStmt::Commit,
        TransactionStmtKind::TransStmtRollback => TransactionStmt::Rollback,
        TransactionStmtKind::TransStmtSavepoint => {
            TransactionStmt::Savepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRelease => {
            TransactionStmt::ReleaseSavepoint(stmt.savepoint_name.clone())
        }
        TransactionStmtKind::TransStmtRollbackTo => {
            TransactionStmt::RollbackToSavepoint(stmt.savepoint_name.clone())
        }
        other => {
            return Err(SQLError::Unsupported(format!("transaction kind {other:?}")));
        }
    };
    Ok(Statement::Transaction(kind))
}

fn compile_update(stmt: &pg_query::protobuf::UpdateStmt) -> Result<UpdateStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("UPDATE without relation".into()))?;
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
    let returning = compile_projections(&stmt.returning_list)?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(UpdateStmt {
        table,
        assignments,
        r#where,
        with,
        from,
        returning,
    })
}

fn compile_delete(stmt: &pg_query::protobuf::DeleteStmt) -> Result<DeleteStmt> {
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("DELETE without relation".into()))?;
    let r#where = stmt
        .where_clause
        .as_ref()
        .map(|w| compile_expr(w))
        .transpose()?;
    let using = match stmt.using_clause.first() {
        Some(node) => Some(compile_from_node(node)?),
        None => None,
    };
    let returning = compile_projections(&stmt.returning_list)?;
    let with = match stmt.with_clause.as_ref() {
        Some(wc) => compile_with_clause(wc)?,
        None => Vec::new(),
    };
    Ok(DeleteStmt {
        table,
        r#where,
        with,
        using,
        returning,
    })
}

pub(super) fn other_node_label(node: &NodeEnum) -> &'static str {
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

/// Lower `DROP FUNCTION` / `DROP PROCEDURE`. Each target arrives as
/// an `ObjectWithArgs`; the argument type list (when spelled) is
/// preserved as a typed signature because routine identity includes
/// `(schema, name, argument types)`.
fn compile_drop_function(
    stmt: &pg_query::protobuf::DropStmt,
    is_procedure: bool,
) -> Result<Statement> {
    use crate::ast::{DropFunctionItem, DropFunctionStmt};
    let mut items = Vec::new();
    for object in &stmt.objects {
        let Some(NodeEnum::ObjectWithArgs(owa)) = object.node.as_ref() else {
            return Err(SQLError::Unsupported(
                "DROP FUNCTION target is not a function signature".into(),
            ));
        };
        let name = compile_qualified_name(
            &owa.objname,
            if is_procedure {
                "DROP PROCEDURE"
            } else {
                "DROP FUNCTION"
            },
        )?;
        let arg_types = if owa.args_unspecified {
            None
        } else {
            Some(
                owa.objargs
                    .iter()
                    .map(|arg| match arg.node.as_ref() {
                        Some(NodeEnum::TypeName(t)) => compile_function_type_name(t),
                        other => Err(SQLError::Unsupported(format!(
                            "DROP FUNCTION argument type node {other:?}"
                        ))),
                    })
                    .collect::<Result<Vec<_>>>()?,
            )
        };
        items.push(DropFunctionItem { name, arg_types });
    }
    if items.is_empty() {
        return Err(SQLError::Internal("DROP FUNCTION without target".into()));
    }
    Ok(Statement::DropFunction(DropFunctionStmt {
        is_procedure,
        if_exists: stmt.missing_ok,
        cascade: matches!(
            stmt.behavior(),
            pg_query::protobuf::DropBehavior::DropCascade
        ),
        items,
    }))
}

fn compile_drop(stmt: &pg_query::protobuf::DropStmt) -> Result<Statement> {
    use pg_query::protobuf::{DropBehavior, ObjectType};
    let kind = match stmt.remove_type() {
        ObjectType::ObjectTable => DropKind::Table,
        ObjectType::ObjectIndex => DropKind::Index,
        ObjectType::ObjectView => DropKind::View,
        ObjectType::ObjectSchema => DropKind::Schema,
        ObjectType::ObjectFunction => return compile_drop_function(stmt, false),
        ObjectType::ObjectProcedure => return compile_drop_function(stmt, true),
        other => {
            return Err(SQLError::Unsupported(format!(
                "DROP target {other:?} not supported"
            )));
        }
    };
    let mut names = Vec::new();
    for object in &stmt.objects {
        let inner = object
            .node
            .as_ref()
            .ok_or_else(|| SQLError::Internal("DROP contains an empty target".into()))?;
        match inner {
            NodeEnum::List(list) => {
                let parts = list
                    .items
                    .iter()
                    .map(extract_string)
                    .collect::<Result<Vec<_>>>()?;
                if parts.is_empty() {
                    return Err(SQLError::Internal("DROP target has no name".into()));
                }
                if matches!(kind, DropKind::Table | DropKind::View) {
                    if parts.len() > 2 {
                        return Err(SQLError::Unsupported(
                            "cross-database DROP targets are not supported".into(),
                        ));
                    }
                    names.push(
                        parts
                            .iter()
                            .map(|part| render_relation_component(part))
                            .collect::<Vec<_>>()
                            .join("."),
                    );
                } else {
                    names.push(parts.last().cloned().unwrap_or_default());
                }
            }
            NodeEnum::String(s) => names.push(s.sval.clone()),
            other => {
                return Err(SQLError::Unsupported(format!(
                    "DROP object node {other:?} not supported"
                )));
            }
        }
    }
    if names.is_empty() {
        return Err(SQLError::Internal("DROP without target name".into()));
    }
    let cascade = matches!(stmt.behavior(), DropBehavior::DropCascade);
    Ok(Statement::Drop(DropStmt {
        kind,
        names,
        if_exists: stmt.missing_ok,
        cascade,
    }))
}

// -------------------------------------------------------------------------
// ALTER TABLE { ADD COLUMN | DROP COLUMN | RENAME COLUMN | RENAME TO }
// -------------------------------------------------------------------------

fn compile_alter_table(stmt: &pg_query::protobuf::AlterTableStmt) -> Result<AlterTableStmt> {
    use pg_query::protobuf::{AlterTableType, DropBehavior};
    let table = stmt
        .relation
        .as_ref()
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without relation".into()))?;
    let if_exists = stmt.missing_ok;
    let cmd = stmt
        .cmds
        .first()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE without command".into()))?;
    let inner = cmd
        .node
        .as_ref()
        .ok_or_else(|| SQLError::Internal("ALTER TABLE command body empty".into()))?;
    let cmd = match inner {
        NodeEnum::AlterTableCmd(c) => c,
        other => {
            return Err(SQLError::Unsupported(format!(
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
                .ok_or_else(|| SQLError::Internal("ADD COLUMN without ColumnDef".into()))?;
            let col_def = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ADD COLUMN expected ColumnDef, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AddColumn {
                column: col_def,
                if_not_exists: cmd.missing_ok,
            }
        }
        AlterTableType::AtAddConstraint => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|definition| definition.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ADD CONSTRAINT without Constraint".into()))?;
            let constraint = match def_inner {
                NodeEnum::Constraint(constraint) => constraint,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ADD CONSTRAINT expected Constraint, got {other:?}"
                    )));
                }
            };
            let kind = match constraint.contype() {
                pg_query::protobuf::ConstrType::ConstrPrimary => TableKeyConstraintKind::PrimaryKey,
                pg_query::protobuf::ConstrType::ConstrUnique => TableKeyConstraintKind::Unique,
                other => {
                    return Err(SQLError::Unsupported(format!(
                        "ALTER TABLE ADD CONSTRAINT {other:?} is not supported"
                    )));
                }
            };
            let columns = constraint
                .keys
                .iter()
                .map(extract_string)
                .collect::<Result<Vec<_>>>()?;
            if columns.is_empty() {
                return Err(SQLError::TypeMismatch(
                    "PRIMARY KEY / UNIQUE constraint must name at least one column".into(),
                ));
            }
            let mut seen = std::collections::BTreeSet::new();
            for column in &columns {
                if !seen.insert(column.as_str()) {
                    return Err(SQLError::TypeMismatch(format!(
                        "PRIMARY KEY / UNIQUE constraint names column `{column}` more than once"
                    )));
                }
            }
            AlterTableAction::AddKeyConstraint {
                constraint: TableKeyConstraint {
                    name: (!constraint.conname.is_empty()).then(|| constraint.conname.clone()),
                    kind,
                    columns,
                    nulls_not_distinct: constraint.nulls_not_distinct,
                },
            }
        }
        AlterTableType::AtDropColumn => AlterTableAction::DropColumn {
            name: cmd.name.clone(),
            if_exists: cmd.missing_ok,
            cascade: matches!(cmd.behavior(), DropBehavior::DropCascade),
        },
        AlterTableType::AtColumnDefault => {
            if let Some(default) = cmd.def.as_deref() {
                AlterTableAction::SetDefault {
                    name: cmd.name.clone(),
                    default: compile_expr(default)?,
                }
            } else {
                AlterTableAction::DropDefault {
                    name: cmd.name.clone(),
                }
            }
        }
        AlterTableType::AtSetNotNull => AlterTableAction::SetNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtDropNotNull => AlterTableAction::DropNotNull {
            name: cmd.name.clone(),
        },
        AlterTableType::AtAlterColumnType => {
            let def_inner = cmd
                .def
                .as_ref()
                .and_then(|d| d.node.as_ref())
                .ok_or_else(|| SQLError::Internal("ALTER COLUMN TYPE without type".into()))?;
            let ty = match def_inner {
                NodeEnum::ColumnDef(c) => compile_column_def(c)?.ty,
                NodeEnum::TypeName(t) => compile_pg_type_name(t, &cmd.name)?,
                other => {
                    return Err(SQLError::Internal(format!(
                        "ALTER COLUMN TYPE expected ColumnDef/TypeName, got {other:?}"
                    )));
                }
            };
            AlterTableAction::AlterColumnType {
                name: cmd.name.clone(),
                ty,
            }
        }
        other => {
            return Err(SQLError::Unsupported(format!(
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
        .map(range_var_name)
        .ok_or_else(|| SQLError::Internal("RENAME without relation".into()))?;
    let action = match stmt.rename_type() {
        ObjectType::ObjectColumn => AlterTableAction::RenameColumn {
            from: stmt.subname.clone(),
            to: stmt.newname.clone(),
        },
        ObjectType::ObjectTable => AlterTableAction::RenameTable {
            to: render_relation_component(&stmt.newname),
        },
        other => {
            return Err(SQLError::Unsupported(format!(
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

pub fn plan_only_for_test(sql: &str) -> Result<Vec<Statement>> {
    compile(sql)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{ColumnType, FromClause, TableKeyConstraintKind};

    fn first(sql: &str) -> Statement {
        let mut v = compile(sql).unwrap();
        assert_eq!(v.len(), 1, "expected 1 stmt");
        v.remove(0)
    }

    fn null_literal_node() -> Node {
        Node {
            node: Some(NodeEnum::AConst(pg_query::protobuf::AConst {
                isnull: true,
                ..Default::default()
            })),
        }
    }

    #[test]
    fn analyze_preserves_its_relation_and_rejects_dropped_semantics() {
        let Statement::Analyze { table } = first("ANALYZE app.docs") else {
            panic!("not ANALYZE");
        };
        assert_eq!(table.as_deref(), Some("app.docs"));
        let Statement::Analyze { table } = first("ANALYZE") else {
            panic!("not ANALYZE");
        };
        assert!(table.is_none());

        for (sql, expected) in [
            ("ANALYZE docs (title)", "column lists"),
            ("ANALYZE (VERBOSE) docs", "options"),
            ("VACUUM docs", "VACUUM"),
        ] {
            let error = compile(sql).expect_err(sql);
            assert!(
                matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
                "unexpected error for {sql}: {error}"
            );
        }
    }

    #[test]
    fn malformed_type_cast_never_degrades_to_the_uncast_expression() {
        let cast = Node {
            node: Some(NodeEnum::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                arg: Some(Box::new(null_literal_node())),
                type_name: None,
                ..Default::default()
            }))),
        };

        let error = compile_expr(&cast).unwrap_err();
        assert!(error.to_string().contains("without a target type"));
    }

    #[test]
    fn malformed_operator_name_is_not_silently_discarded() {
        let expression = Node {
            node: Some(NodeEnum::AExpr(Box::new(pg_query::protobuf::AExpr {
                kind: pg_query::protobuf::AExprKind::AexprOp as i32,
                name: vec![Node::default()],
                lexpr: Some(Box::new(null_literal_node())),
                rexpr: Some(Box::new(null_literal_node())),
                ..Default::default()
            }))),
        };

        let error = compile_expr(&expression).unwrap_err();
        assert!(error.to_string().contains("missing string node"));
    }

    #[test]
    fn sequence_options_do_not_truncate_or_ignore_values() {
        assert!(compile("CREATE SEQUENCE s START 1.5").is_err());
        let error = compile("CREATE SEQUENCE s CACHE 10").unwrap_err();
        assert!(error.to_string().contains("not supported"));
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
    fn create_table_preserves_boolean_column_type() {
        let Statement::CreateTable(table) = first("CREATE TABLE flags (enabled BOOLEAN)") else {
            panic!("not CREATE TABLE");
        };
        assert!(matches!(table.columns[0].ty, ColumnType::Boolean));
    }

    #[test]
    fn create_table_preserves_array_element_types_and_dimensions() {
        let Statement::CreateTable(table) =
            first("CREATE TABLE arrays (tags TEXT[], matrix INTEGER[][])")
        else {
            panic!("not CREATE TABLE");
        };
        assert_eq!(
            table.columns[0].ty,
            ColumnType::Array(Box::new(ColumnType::Text))
        );
        assert_eq!(
            table.columns[1].ty,
            ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer))))
        );
    }

    #[test]
    fn create_table_preserves_typed_composite_keys_and_null_policy() {
        let Statement::CreateTable(table) = first(
            "CREATE TABLE memberships (
                tenant TEXT,
                member TEXT,
                email TEXT,
                CONSTRAINT memberships_pkey PRIMARY KEY (tenant, member),
                CONSTRAINT memberships_email_key UNIQUE NULLS NOT DISTINCT (tenant, email)
            )",
        ) else {
            panic!("not CREATE TABLE");
        };

        assert_eq!(table.key_constraints.len(), 2);
        assert_eq!(
            table.key_constraints[0].kind,
            TableKeyConstraintKind::PrimaryKey
        );
        assert_eq!(table.key_constraints[0].columns, vec!["tenant", "member"]);
        assert_eq!(
            table.key_constraints[0].name.as_deref(),
            Some("memberships_pkey")
        );
        assert_eq!(
            table.key_constraints[1].kind,
            TableKeyConstraintKind::Unique
        );
        assert_eq!(table.key_constraints[1].columns, vec!["tenant", "email"]);
        assert!(table.key_constraints[1].nulls_not_distinct);

        assert!(table.columns[0].not_null);
        assert!(table.columns[1].not_null);
        assert!(!table.columns[0].primary_key);
        assert!(!table.columns[1].primary_key);
    }

    #[test]
    fn create_table_preserves_named_column_keys() {
        let Statement::CreateTable(table) = first(
            "CREATE TABLE users (
                id INTEGER CONSTRAINT users_pkey PRIMARY KEY,
                email TEXT CONSTRAINT users_email_key UNIQUE
            )",
        ) else {
            panic!("not CREATE TABLE");
        };
        assert_eq!(table.key_constraints.len(), 2);
        assert_eq!(table.key_constraints[0].name.as_deref(), Some("users_pkey"));
        assert_eq!(
            table.key_constraints[1].name.as_deref(),
            Some("users_email_key")
        );
        assert!(table.columns[0].not_null);
    }

    #[test]
    fn create_table_rejects_invalid_key_declarations() {
        for sql in [
            "CREATE TABLE t (a INTEGER, CONSTRAINT same UNIQUE (a), CONSTRAINT same CHECK (a > 0))",
            "CREATE TABLE t (a INTEGER, UNIQUE (missing))",
            "CREATE TABLE t (a INTEGER, UNIQUE (a, a))",
            "CREATE TABLE t (a INTEGER PRIMARY KEY, b INTEGER, PRIMARY KEY (b))",
        ] {
            assert!(compile(sql).is_err(), "expected invalid DDL to fail: {sql}");
        }
    }

    #[test]
    fn explicit_grouping_sets_preserve_every_key_expression() {
        let Statement::Select(select) =
            first("SELECT g, v, count(*) FROM spill_data GROUP BY GROUPING SETS ((g), (v), ())")
        else {
            panic!("not SELECT");
        };
        assert_eq!(
            select.grouping_sets.len(),
            3,
            "compiled grouping sets: {:?}",
            select.grouping_sets
        );
        assert_eq!(select.grouping_sets[0].len(), 1);
        assert_eq!(select.grouping_sets[1].len(), 1);
        assert!(select.grouping_sets[2].is_empty());
    }

    #[test]
    fn rollup_cube_and_multiple_grouping_items_expand_without_dropping_keys() {
        let Statement::Select(rollup) =
            first("SELECT g, v, count(*) FROM t GROUP BY ROLLUP (g, v)")
        else {
            panic!("not SELECT");
        };
        assert_eq!(
            rollup
                .grouping_sets
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![2, 1, 0]
        );

        let Statement::Select(cube) = first("SELECT g, v, count(*) FROM t GROUP BY CUBE (g, v)")
        else {
            panic!("not SELECT");
        };
        let mut cube_widths = cube.grouping_sets.iter().map(Vec::len).collect::<Vec<_>>();
        cube_widths.sort_unstable();
        assert_eq!(cube_widths, vec![0, 1, 1, 2]);

        let Statement::Select(product) = first(
            "SELECT a, b, c, d, count(*) FROM t \
             GROUP BY GROUPING SETS ((a), (b)), GROUPING SETS ((c), (d))",
        ) else {
            panic!("not SELECT");
        };
        assert_eq!(product.grouping_sets.len(), 4);
        assert!(product.grouping_sets.iter().all(|set| set.len() == 2));
    }

    #[test]
    fn create_table_with_tensor_column() {
        let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(4))");
        let Statement::CreateTable(ct) = stmt else {
            panic!("not CREATE TABLE");
        };
        assert!(matches!(ct.columns[1].ty, ColumnType::Tensor(4)));
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
    fn table_commands_preserve_qualified_relation_names() {
        let stmt = first("ALTER TABLE app.docs ADD COLUMN version INTEGER");
        let Statement::AlterTable(alter) = stmt else {
            panic!("not ALTER TABLE");
        };
        assert_eq!(alter.table, "app.docs");

        let stmt = first("ALTER TABLE app.docs RENAME TO archived_docs");
        let Statement::AlterTable(rename) = stmt else {
            panic!("not ALTER TABLE RENAME");
        };
        assert_eq!(rename.table, "app.docs");

        let Statement::Update(update) = first("UPDATE app.docs SET version = 2") else {
            panic!("not UPDATE");
        };
        assert_eq!(update.table, "app.docs");

        let Statement::Delete(delete) = first("DELETE FROM app.docs") else {
            panic!("not DELETE");
        };
        assert_eq!(delete.table, "app.docs");

        let Statement::Truncate { tables, .. } = first("TRUNCATE app.docs") else {
            panic!("not TRUNCATE");
        };
        assert_eq!(tables, vec!["app.docs"]);

        let Statement::Insert(insert) = first("INSERT INTO app.docs (version) VALUES (1)") else {
            panic!("not INSERT");
        };
        assert_eq!(insert.table, "app.docs");
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
        match &s.r#where {
            Some(Expr::Func {
                distinct: false,
                order_by,
                filter: None,
                ..
            }) if order_by.is_empty() => {}
            other => panic!("expected scalar function call, got {other:?}"),
        }
        assert_eq!(s.order_by.len(), 1);
        assert!(s.order_by[0].descending);
        match &s.limit {
            Some(Expr::Literal(uqa_core::Value::Int(5))) => {}
            other => panic!("expected LIMIT 5, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_select_clauses_fail_instead_of_losing_semantics() {
        for (sql, expected) in [
            ("SELECT 1 INTO created_by_select", "SELECT INTO"),
            (
                "SELECT department, count(*) FROM employees GROUP BY DISTINCT department",
                "GROUP BY DISTINCT",
            ),
            (
                "SELECT row_number() OVER named_window FROM employees WINDOW named_window AS (ORDER BY id)",
                "named WINDOW",
            ),
            (
                "SELECT * FROM employees ORDER BY id FETCH FIRST 1 ROW WITH TIES",
                "WITH TIES",
            ),
            ("SELECT * FROM employees FOR UPDATE", "row-locking"),
        ] {
            let error = compile(sql).expect_err(sql);
            assert!(
                matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
                "unexpected error for {sql}: {error}"
            );
        }
    }

    #[test]
    fn unsupported_from_forms_fail_instead_of_becoming_cross_joins() {
        for (sql, expected) in [
            (
                "SELECT * FROM left_table NATURAL JOIN right_table",
                "NATURAL JOIN",
            ),
            (
                "SELECT * FROM left_table JOIN right_table USING (id)",
                "JOIN USING",
            ),
            (
                "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(3, 4)) AS f(a, b)",
                "ROWS FROM",
            ),
            (
                "SELECT * FROM generate_series(1, 2) WITH ORDINALITY",
                "WITH ORDINALITY",
            ),
        ] {
            let error = compile(sql).expect_err(sql);
            assert!(
                matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
                "unexpected error for {sql}: {error}"
            );
        }
    }

    #[test]
    fn unsupported_cte_control_clauses_fail_explicitly() {
        let not_materialized =
            compile("WITH c AS NOT MATERIALIZED (SELECT 1) SELECT * FROM c").unwrap_err();
        assert!(matches!(
            not_materialized,
            SQLError::Unsupported(message) if message.contains("NOT MATERIALIZED")
        ));

        let search = compile(
            "WITH RECURSIVE t(n) AS (VALUES (1) UNION ALL SELECT n + 1 FROM t WHERE n < 3) \
             SEARCH DEPTH FIRST BY n SET ordering SELECT * FROM t",
        )
        .unwrap_err();
        assert!(matches!(
            search,
            SQLError::Unsupported(message) if message.contains("SEARCH")
        ));

        let cycle = compile(
            "WITH RECURSIVE t(n) AS (VALUES (1) UNION ALL SELECT n + 1 FROM t WHERE n < 3) \
             CYCLE n SET is_cycle USING path SELECT * FROM t",
        )
        .unwrap_err();
        assert!(matches!(
            cycle,
            SQLError::Unsupported(message) if message.contains("CYCLE")
        ));
    }

    #[test]
    fn quoted_dots_preserve_range_var_component_boundaries() {
        let Statement::CreateTable(table) = first("CREATE TABLE \"a.b\".c (id INTEGER)") else {
            panic!("expected CREATE TABLE");
        };
        assert_eq!(table.name, "\"a.b\".c");

        let Statement::Select(select) = first("SELECT * FROM a.\"b.c\"") else {
            panic!("expected SELECT");
        };
        assert!(matches!(
            select.from,
            Some(FromClause::Table { name, .. }) if name == "a.\"b.c\""
        ));

        let Statement::AlterTable(alter) = first("ALTER TABLE \"a.b\".c RENAME TO \"d.e\"") else {
            panic!("expected ALTER TABLE");
        };
        assert!(matches!(
            alter.action,
            AlterTableAction::RenameTable { to } if to == "\"d.e\""
        ));

        let Statement::Drop(drop) = first("DROP TABLE \"a.b\".\"d.e\"") else {
            panic!("expected DROP TABLE");
        };
        assert_eq!(drop.names, vec!["\"a.b\".\"d.e\"".to_string()]);
    }

    #[test]
    fn alter_table_add_key_constraint_preserves_tuple_shape() {
        let Statement::AlterTable(alter) =
            first("ALTER TABLE labels ADD CONSTRAINT labels_tenant_slug_key UNIQUE (tenant, slug)")
        else {
            panic!("expected ALTER TABLE");
        };
        assert!(matches!(
            alter.action,
            AlterTableAction::AddKeyConstraint { constraint }
                if constraint.name.as_deref() == Some("labels_tenant_slug_key")
                    && constraint.kind == TableKeyConstraintKind::Unique
                    && constraint.columns == ["tenant", "slug"]
        ));
    }

    #[test]
    fn alter_sequence_preserves_if_exists() {
        let Statement::AlterSequence(sequence) =
            first("ALTER SEQUENCE IF EXISTS absent RESTART WITH 7")
        else {
            panic!("expected ALTER SEQUENCE");
        };
        assert!(sequence.if_exists);
        assert_eq!(sequence.restart, crate::ast::SequenceRestart::With(7));
    }

    #[test]
    fn unsupported_create_ddl_never_loses_lifecycle_semantics() {
        for (sql, expected) in [
            ("CREATE TEMP TABLE temp_t (id INTEGER)", "TEMPORARY"),
            ("CREATE UNLOGGED TABLE unlogged_t (id INTEGER)", "UNLOGGED"),
            (
                "CREATE TABLE inherited (id INTEGER) INHERITS (parent)",
                "INHERITS",
            ),
            (
                "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
                "storage options",
            ),
            (
                "CREATE TABLE spaced (id INTEGER) TABLESPACE fastspace",
                "TABLESPACE",
            ),
            (
                "CREATE TABLE accessed (id INTEGER) USING heap",
                "access methods",
            ),
            (
                "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
                "AUTHORIZATION",
            ),
            (
                "CREATE SCHEMA bundled CREATE TABLE child (id INTEGER)",
                "schema elements",
            ),
            ("CREATE TEMP VIEW temp_v AS SELECT 1", "TEMPORARY"),
            ("CREATE VIEW aliased(value) AS SELECT 1", "column aliases"),
            (
                "CREATE VIEW checked AS SELECT 1 WITH LOCAL CHECK OPTION",
                "CHECK OPTION",
            ),
            (
                "CREATE VIEW optioned_v WITH (security_barrier = true) AS SELECT 1",
                "options",
            ),
            (
                "CREATE MATERIALIZED VIEW materialized AS SELECT 1",
                "MATERIALIZED VIEW",
            ),
            ("CREATE TEMP TABLE temp_as AS SELECT 1", "TEMPORARY"),
            ("CREATE TABLE named(value) AS SELECT 1", "column-name lists"),
            (
                "CREATE TABLE no_data AS SELECT 1 WITH NO DATA",
                "WITH NO DATA",
            ),
            ("CREATE TEMP SEQUENCE temp_sequence", "TEMPORARY"),
        ] {
            let error = compile(sql).expect_err(sql);
            assert!(
                matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
                "unexpected error for {sql}: {error}"
            );
        }
    }
}
