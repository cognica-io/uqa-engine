//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite-rule definition rendering.

use uqa_sql::ast::CreateRule;
use uqa_sql::SQLError;

use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;

/// `pg_get_ruledef`: the rule as `make_ruledef` prints it, naming its relation qualified unless pretty output finds it visible.
pub fn render_rule_definition(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    definition: &CreateRule,
    pretty: bool,
) -> Result<String, SQLError> {
    let relation = render_rule_relation(catalog, resolution, &definition.table, pretty)?;
    super::super::view_definition::rule_definition(
        catalog, resolution, definition, &relation, pretty,
    )
}

pub fn render_rule_relation(
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
