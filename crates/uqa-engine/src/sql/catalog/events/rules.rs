//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule definition rendering.

use uqa_sql::ast::{CreateRule, Expr, RuleEvent};
use uqa_sql::SQLError;

use crate::engine_capabilities::{CatalogReadView, RelationNameResolution};
use crate::RelationIdentity;

pub(super) fn render_rule_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    definition: &CreateRule,
    pretty: bool,
) -> Result<String, SQLError> {
    let mut rendered = format!(
        "CREATE RULE {} AS ON {} TO {}",
        uqa_sql::expr::quote_ident(&definition.name),
        match definition.event {
            RuleEvent::Select => "SELECT",
            RuleEvent::Insert => "INSERT",
            RuleEvent::Update => "UPDATE",
            RuleEvent::Delete => "DELETE",
        },
        render_rule_relation(catalog, resolution, &definition.table, pretty)?
    );
    if let Some(condition) = rule_condition_text(definition, pretty) {
        rendered.push_str(" WHERE (");
        rendered.push_str(&condition);
        rendered.push(')');
    }
    rendered.push_str(if definition.instead {
        " DO INSTEAD"
    } else {
        " DO ALSO"
    });
    match definition.action_sql.as_slice() {
        [] => rendered.push_str(" NOTHING"),
        [action] => {
            rendered.push(' ');
            rendered.push_str(action);
        }
        actions => {
            rendered.push_str(" (");
            rendered.push_str(&actions.join("; "));
            rendered.push_str(";)");
        }
    }
    Ok(rendered)
}

pub(super) fn rule_condition_text(definition: &CreateRule, pretty: bool) -> Option<String> {
    let condition = definition.condition.as_ref()?;
    let has_subquery = condition.any_node(&|node| {
        matches!(
            node,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        )
    });
    if has_subquery {
        if let Some(sql) = definition.condition_sql.as_ref() {
            return Some(sql.clone());
        }
    }
    Some(super::render_trigger_condition(condition, pretty))
}

pub(super) fn render_rule_relation(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    pretty: bool,
) -> Result<String, SQLError> {
    let relation = RelationIdentity::from_legacy_name(name)
        .map_err(|error| SQLError::Internal(format!("decode rule relation `{name}`: {error}")))?;
    if pretty {
        let local = uqa_sql::expr::quote_ident(&relation.name);
        let visible_table = catalog.table_name_resolved(resolution, &local)?;
        let visible_view = catalog.view_name_resolved(resolution, &local)?;
        if visible_table.as_deref() == Some(name) || visible_view.as_deref() == Some(name) {
            return Ok(local);
        }
    }
    Ok(super::render_qualified_name(name))
}
