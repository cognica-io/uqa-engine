//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite literal sequence references in stored table and view declarations.
use crate::schema::dependencies::rewrites::{
    rewrite_sequence_function_references, stored_relation_reference_matches,
};
use uqa_core::RelationIdentity;
pub fn rewrite_sequence_schema_references(
    columns: &mut [crate::ast::ColumnDef],
    checks: &mut [crate::ast::TableCheck],
    from: &RelationIdentity,
    to: &str,
) -> Result<bool, String> {
    let mut changed = false;
    for column in columns {
        if let Some(sequence) = column
            .auto_increment
            .as_mut()
            .and_then(|provenance| provenance.sequence.as_mut())
        {
            if stored_relation_reference_matches(sequence, from) {
                *sequence = to.to_string();
                changed = true;
            }
        }
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            rewrite_sequence_function_references(expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
        if let Some(generated) = &mut column.generated {
            rewrite_sequence_function_references(&mut generated.expression, &mut |reference| {
                if stored_relation_reference_matches(reference, from) {
                    *reference = to.to_string();
                    changed = true;
                }
                Ok(())
            })?;
        }
    }
    for check in checks {
        rewrite_sequence_function_references(&mut check.expr, &mut |reference| {
            if stored_relation_reference_matches(reference, from) {
                *reference = to.to_string();
                changed = true;
            }
            Ok(())
        })?;
    }
    Ok(changed)
}

pub fn rewritten_view_sequence_references(
    stored: &crate::catalog::stored_view::StoredView,
    from: &RelationIdentity,
    to: &str,
) -> Result<Option<crate::catalog::stored_view::StoredView>, String> {
    let mut rewritten = stored.clone();
    let mut changed = false;
    crate::binding::view_dependencies::bind_query_plan_sequence_references(
        &mut rewritten.query,
        &mut |reference| -> Result<String, String> {
            let (schema, name) = RelationIdentity::parse_reference(reference).map_err(|error| {
                format!("invalid stored view sequence reference `{reference}`: {error}")
            })?;
            let matches = schema.as_deref().map_or(name == from.name, |schema| {
                schema == from.schema && name == from.name
            });
            if matches {
                changed = true;
                Ok(to.to_string())
            } else {
                Ok(reference.to_string())
            }
        },
    )?;
    Ok(changed.then_some(rewritten))
}

pub mod analysis;

pub fn detach_sequence_provenance(columns: &mut [crate::ast::ColumnDef], sequence: &str) -> bool {
    let mut changed = false;
    for column in columns {
        if column
            .auto_increment
            .as_ref()
            .is_some_and(|provenance| provenance.sequence.as_deref() == Some(sequence))
        {
            column.auto_increment = None;
            changed = true;
        }
    }
    changed
}
