//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inspect stored sequence dependencies under borrowed catalog identity guards.
use super::super::{dependents::SequenceSchemaDependent, implicit_ownership::StoredSequenceNames};
use crate::schema::dependencies::rewrites::rewrite_sequence_function_references;
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;
pub type SequenceExpressionObjectIdsRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, [u8; 16]>> + 'a>;
pub trait SequenceExpressionCatalog: StoredSequenceNames {
    fn object_ids(&self) -> SequenceExpressionObjectIdsRead<'_>;
}
fn expression_references_sequence(
    catalog: &dyn SequenceExpressionCatalog,
    expression: Option<&crate::ast::Expr>,
    sequence: &str,
) -> Result<bool, String> {
    expression.map_or(Ok(false), |expression| {
        Ok(stored_sequence_targets_in_loaded_expr(catalog, expression)?.contains(sequence))
    })
}
pub fn append_sequence_schema_expression_dependents(
    catalog: &dyn SequenceExpressionCatalog,
    table_name: &str,
    columns: &[crate::ast::ColumnDef],
    checks: &[crate::ast::TableCheck],
    sequence: &str,
    foreign: bool,
    dependents: &mut Vec<SequenceSchemaDependent>,
) -> Result<(), String> {
    let relation = if foreign {
        format!("foreign table `{table_name}`")
    } else {
        format!("`{table_name}`")
    };
    for column in columns {
        if expression_references_sequence(catalog, column.default.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::Default {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(
            catalog,
            column
                .generated
                .as_ref()
                .map(|generated| generated.expression.as_ref()),
            sequence,
        )? {
            dependents.push(SequenceSchemaDependent::GeneratedColumn {
                table: table_name.to_string(),
                column: column.name.clone(),
                foreign,
            });
        }
        if expression_references_sequence(catalog, column.check.as_ref(), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: column.check_name.clone().ok_or_else(|| {
                    format!(
                        "CHECK constraint on {relation}.`{}` has no catalog name",
                        column.name
                    )
                })?,
                foreign,
            });
        }
    }
    for check in checks {
        if expression_references_sequence(catalog, Some(&check.expr), sequence)? {
            dependents.push(SequenceSchemaDependent::CheckConstraint {
                table: table_name.to_string(),
                constraint: check.name.clone().ok_or_else(|| {
                    format!("table CHECK constraint on {relation} has no catalog name")
                })?,
                foreign,
            });
        }
    }
    Ok(())
}
pub fn stored_sequence_targets_in_loaded_expr(
    catalog: &dyn SequenceExpressionCatalog,
    expression: &crate::ast::Expr,
) -> Result<std::collections::BTreeSet<String>, String> {
    let mut expression = expression.clone();
    let mut targets = std::collections::BTreeSet::new();
    rewrite_sequence_function_references(&mut expression, &mut |reference| {
        let canonical = catalog.stored_sequence_name(reference)?;
        targets.insert(canonical.clone());
        *reference = canonical;
        Ok(())
    })?;
    let identities = catalog
        .object_ids()
        .iter()
        .map(|(name, id)| {
            (
                crate::catalog::oids::stable_object_oid("relation", id),
                name.qualified_name(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    crate::schema::dependencies::walk_schema_expr_mut(&mut expression, &mut |node| {
        if let Some(name) = crate::schema::dependencies::regclass::regclass_constant_oid(node)
            .and_then(|oid| identities.get(&oid))
        {
            targets.insert(name.clone());
        }
        Ok(())
    })?;
    Ok(targets)
}

#[cfg(test)]
mod tests;
