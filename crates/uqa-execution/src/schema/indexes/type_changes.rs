//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Revalidate affected B-tree declarations after all column types have changed and before rewriting their rows.

use super::registry::IndexRegistryContext;
use uqa_sql::{
    catalog::index::stored, schema::dependencies::registration::SchemaDependencyBindingContext,
    SQLError,
};

/// The ALTER scheduler holds the table's definition lock and enclosing statement transaction. Read the current private schema, so a failure rolls back every type change in the statement; included values are not key attributes.
pub(crate) fn validate(
    context: &IndexRegistryContext<'_>,
    bindings: &SchemaDependencyBindingContext<'_>,
    table: &str,
    changed_columns: &[&str],
) -> Result<(), SQLError> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    for row in catalog
        .catalog_indexes()
        .filter(|row| row.table_name == table && matches!(row.index_type.as_str(), "" | "btree"))
    {
        let mut affected = false;
        for column in changed_columns {
            affected |= stored::references_column(
                &row.columns_json,
                row.definition_json.as_deref(),
                column,
            )
            .map_err(|error| {
                SQLError::Internal(format!("read altered index references: {error}"))
            })?;
        }
        if affected {
            let mut declaration = stored::declaration(row).map_err(|error| {
                SQLError::Internal(format!("read altered index declaration: {error}"))
            })?;
            uqa_sql::schema::indexes::keys::prepare_index_definition(
                bindings.schema,
                bindings.bindings,
                &mut declaration,
            )?;
        }
    }
    Ok(())
}
