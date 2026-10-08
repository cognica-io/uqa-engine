//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename a composite attribute while retaining its immutable catalog identities.

use super::alteration::CompositeAlterationContext;
use crate::schema::table_alteration::entry::TableAlterEntryContext;
use uqa_sql::{
    ast::{AlterTableAction, AlterTableStmt},
    expr::composites::AttributeChange,
    SQLError, SQLResult,
};

pub fn rename_attribute<S: Clone + 'static>(
    context: &CompositeAlterationContext<'_>,
    relations: &TableAlterEntryContext<'_, S>,
    name: &str,
    from: &str,
    to: &str,
) -> Result<SQLResult, SQLError> {
    let target = uqa_sql::schema::relation_alteration::RelationAlterTarget::resolve(
        context.binding.names.resolve_relation_kind(name)?,
        name,
        false,
        &mut |_| {},
    )?
    .ok_or_else(|| SQLError::UnknownTable(name.to_owned()))?;
    // PostgreSQL's RENAME ATTRIBUTE spelling shares renameatt with ordinary relation columns.
    if target.kind != "composite type" {
        return crate::schema::table_alteration::entry::run_alter_table(
            relations,
            AlterTableStmt {
                table: name.to_owned(),
                qualifier: target.relation.name,
                if_exists: false,
                recurse: true,
                actions: vec![AlterTableAction::RenameColumn {
                    from: from.to_owned(),
                    to: to.to_owned(),
                }],
            },
        );
    }
    let definition = super::alteration::bind(context, name)?;
    context.binding.locks.prepare_definition_write()?;
    uqa_sql::schema::composites::validate_renamed_attribute(&definition, from, to)?;
    let rebuild = super::values::rewrite_composite_values(
        &context.attributes.values,
        definition.oid,
        &AttributeChange::Rename {
            from: from.to_owned(),
            to: to.to_owned(),
        },
    )?;
    let before = context.attributes.publication.composite_registry().clone();
    let mut after = before.clone();
    let attribute = after
        .get_mut(&definition.identity.qualified_name())
        .and_then(|definition| {
            definition
                .attributes
                .iter_mut()
                .find(|attribute| !attribute.dropped && attribute.name == from)
        })
        .ok_or_else(|| SQLError::Internal("renamed composite attribute disappeared".into()))?;
    to.clone_into(&mut attribute.name);
    crate::catalog::composite_type::publish(context.attributes.publication, &before, after)?;
    context.attributes.changes.catalog_registry_changed();
    super::values::rebuild_indexes(&context.attributes.values, rebuild)?;
    Ok(SQLResult::empty())
}
