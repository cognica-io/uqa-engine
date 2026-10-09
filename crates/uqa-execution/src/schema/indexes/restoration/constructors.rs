//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain composite constructor positions in validated index keys and predicates before loading physical indexes.

use super::{invalid, CatalogFacade, RestoredIndexCatalog, StorageBackendResult};
use uqa_sql::{ast::IndexKey, schema::dependencies::registration::SchemaDependencyBindingContext};

pub fn restore_constructors(
    storage: &dyn CatalogFacade,
    context: &SchemaDependencyBindingContext<'_>,
    restored: &mut RestoredIndexCatalog,
    allow_migration: bool,
) -> StorageBackendResult<()> {
    let mut updates = Vec::new();
    for row in &restored.rows {
        let mut keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json)?;
        let mut definition = crate::catalog::index::index_definition(row)?;
        let mut changed = false;
        for (expression, predicate) in keys
            .iter_mut()
            .filter_map(|key| match key {
                IndexKey::Expression(expression) => Some((expression.as_mut(), false)),
                IndexKey::Column(_) => None,
            })
            .chain(
                definition
                    .predicate
                    .as_deref_mut()
                    .map(|expression| (expression, true)),
            )
        {
            let normalized = expression.upgrade_legacy_serialized_dispatches();
            if normalized && !allow_migration {
                return Err(invalid(
                    "index expressions require an initial-open migration",
                ));
            }
            changed |= normalized;
            if !uqa_sql::type_resolution::composite_rows::expression_requires_binding(
                expression,
                context.schema,
            )
            .map_err(invalid)?
            {
                continue;
            }
            if !allow_migration {
                return Err(invalid(
                    "index constructors require an initial-open migration",
                ));
            }
            let binding = context.bindings.binding_scope().map_err(invalid)?;
            if predicate {
                uqa_sql::schema::indexes::prepare_index_predicate(
                    context.schema,
                    &binding.context(),
                    &row.table_name,
                    expression,
                )
                .map_err(invalid)?;
            } else {
                uqa_sql::schema::indexes::prepare_index_expression(
                    context.schema,
                    &binding.context(),
                    &row.table_name,
                    expression,
                )
                .map_err(invalid)?;
            }
            changed = true;
        }
        if changed {
            let mut row = row.clone();
            row.columns_json = serde_json::to_string(&keys)?;
            row.definition_json = Some(serde_json::to_string(&definition)?);
            updates.push(row);
        }
    }
    for row in updates {
        storage.save_catalog_index_row(&row)?;
        for current in restored
            .rows
            .iter_mut()
            .chain(&mut restored.builds)
            .filter(|current| current.relation == row.relation)
        {
            *current = row.clone();
        }
    }
    Ok(())
}
