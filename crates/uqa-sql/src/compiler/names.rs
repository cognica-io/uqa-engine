//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualified-name rendering and durable relation-envelope validation.

use super::{extract_string, Node, RangeVar, Result, SQLError};
use crate::ast::{OnCommitAction, RelationPersistence};

pub(crate) fn range_var_name(r: &RangeVar) -> String {
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
pub(crate) fn relation_persistence(
    relation: &RangeVar,
    statement: &str,
) -> Result<RelationPersistence> {
    if !relation.catalogname.is_empty() {
        return Err(SQLError::Unsupported(format!(
            "{statement}: cross-database relation names are not supported"
        )));
    }
    match relation.relpersistence.as_str() {
        "" | "p" => Ok(RelationPersistence::Permanent),
        "t" => Ok(RelationPersistence::Temporary),
        "u" => Ok(RelationPersistence::Unlogged),
        other => Err(SQLError::Unsupported(format!(
            "{statement}: relation persistence `{other}` is not supported"
        ))),
    }
}

pub(crate) fn compile_on_commit(
    action: pg_query::protobuf::OnCommitAction,
    persistence: RelationPersistence,
    statement: &str,
) -> Result<OnCommitAction> {
    use pg_query::protobuf::OnCommitAction as PgOnCommitAction;

    if persistence != RelationPersistence::Temporary
        && !matches!(
            action,
            PgOnCommitAction::Undefined | PgOnCommitAction::OncommitNoop
        )
    {
        return Err(SQLError::Routine {
            sqlstate: "42P16".into(),
            message: format!("ON COMMIT can only be used on temporary tables in {statement}"),
        });
    }
    Ok(match action {
        PgOnCommitAction::Undefined
        | PgOnCommitAction::OncommitNoop
        | PgOnCommitAction::OncommitPreserveRows => OnCommitAction::PreserveRows,
        PgOnCommitAction::OncommitDeleteRows => OnCommitAction::DeleteRows,
        PgOnCommitAction::OncommitDrop => OnCommitAction::Drop,
    })
}

pub(crate) fn validate_create_table_envelope(
    stmt: &pg_query::protobuf::CreateStmt,
    statement: &str,
) -> Result<()> {
    let relation = stmt
        .relation
        .as_ref()
        .ok_or_else(|| SQLError::Internal(format!("{statement} without relation")))?;
    let persistence = relation_persistence(relation, statement)?;
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
    compile_on_commit(stmt.oncommit(), persistence, statement)?;
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

pub(super) fn render_relation_component(component: &str) -> String {
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

pub(crate) fn compile_qualified_name(parts: &[Node], statement: &str) -> Result<String> {
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
