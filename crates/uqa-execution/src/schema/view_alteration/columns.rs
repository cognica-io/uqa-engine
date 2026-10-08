//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a view column rename through the existing dependency owners.

use super::{StoredView, ViewAlterContext};
use uqa_core::RelationIdentity;
use uqa_sql::SQLError;

pub struct ViewColumnRenameContext<'a> {
    pub views: crate::schema::view_references::ViewReferenceContext<'a>,
    pub routines: crate::routines::rewrites::RoutineRewriteContext<'a>,
    pub events: crate::schema::events::context::EventLifecycleContext<'a>,
    pub values: crate::schema::composites::values::CompositeValueContext<'a>,
}

pub trait ViewColumnRenameInputs {
    fn view_column_rename_context(&self) -> ViewColumnRenameContext<'_>;
}

pub(super) fn rename(
    context: &ViewAlterContext<'_>,
    relation: &RelationIdentity,
    kind: &str,
    mut view: StoredView,
    from: &str,
    to: &str,
) -> Result<(), SQLError> {
    let row_type = view
        .relation_oids()
        .row_type
        .ok_or_else(|| SQLError::Internal("view has no row type identity".into()))?;
    let columns = view
        .output_columns
        .as_mut()
        .ok_or_else(|| SQLError::Internal("view has no retained output names".into()))?;
    let position = uqa_sql::schema::columns::renamed_column_position(
        &relation.name,
        columns.iter().map(String::as_str),
        from,
        to,
        kind == "materialized view",
    )?;
    let dependencies = context.column_dependencies.view_column_rename_context();
    let rebuild = crate::schema::composites::values::rewrite_composite_values(
        &dependencies.values,
        row_type,
        &uqa_sql::expr::composites::AttributeChange::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        },
    )?;
    to.clone_into(&mut columns[position]);
    view.security.rename_column_acl(from, to);
    for row in &mut view.materialized_rows {
        if let Some(value) = row.remove(from) {
            row.insert(to.to_owned(), value);
        }
    }
    let canonical = relation.qualified_name();
    crate::schema::view_references::rewrite_view_column_references(
        &dependencies.views,
        &canonical,
        from,
        to,
    )
    .map_err(|error| {
        uqa_sql::catalog::errors::storage_error("rename dependent view column", &error)
    })?;
    super::publish_altered_view(context, relation, &canonical, kind, view)?;
    crate::routines::rewrites::rewrite_routine_column_references(
        &dependencies.routines,
        relation,
        from,
        to,
    )?;
    dependencies
        .events
        .rename_event_column_inner(&canonical, from, to)
        .map_err(|error| uqa_sql::catalog::errors::storage_error("rename event column", &error))?;
    context.changes.catalog_registry_changed();
    crate::schema::composites::values::rebuild_indexes(&dependencies.values, rebuild)
}
