//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-independent catalog source binding.

use crate::ast::OperatorJoinRelations;
use crate::RowSchema;
use crate::SQLError;

use crate::catalog::analysis::CatalogReadView;
use crate::catalog::resolution::RelationNameResolution;

use super::analysis;

/// Bind both operator-join relations independently so each retrieval operand has its own namespace.
pub(super) fn operator_join_relation_schemas(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relations: Option<&OperatorJoinRelations>,
) -> Result<(RowSchema, RowSchema), SQLError> {
    let relations = relations.ok_or_else(|| {
        SQLError::TypeMismatch("operator join requires left and right table identifiers".into())
    })?;
    Ok((
        relation_schema(catalog, resolution, &relations.left, "left")?,
        relation_schema(catalog, resolution, &relations.right, "right")?,
    ))
}

fn relation_schema(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &str,
    side: &str,
) -> Result<RowSchema, SQLError> {
    let resolved = catalog
        .table_name_resolved(resolution, relation)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let identity = crate::RelationIdentity::from_legacy_name(&resolved).map_err(|error| {
        SQLError::Internal(format!(
            "decode operator join {side} relation `{resolved}` schema: {error}"
        ))
    })?;
    let table = catalog
        .table_resolved(resolution, &resolved)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_string()))?;
    let columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect();
    let types = table
        .columns
        .iter()
        .map(|column| Some(column.ty.clone()))
        .collect();
    let schema = RowSchema::with_qualified_types(&identity.name, columns, types);
    Ok(analysis::with_table_pseudo_columns(&schema, &identity.name))
}

/// Output column names of a user-defined routine used as a FROM source: OUT / INOUT / `RETURNS TABLE` parameter names. `None` when the name is not a user routine or its result is a single unnamed column, which keeps the function-name default.
pub fn user_function_output_columns(
    catalog: &dyn crate::catalog::analysis::AnalysisCatalog,
    resolution: &RelationNameResolution,
    name: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    let Some(overloads) = catalog.sql_functions(resolution, name)? else {
        return Ok(None);
    };
    for function in &overloads {
        if let Some(columns) = crate::semantics::user_function_output_columns_for(function) {
            return Ok(Some(columns));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::ast::{ColumnDef, ColumnType};

    use super::operator_join_relation_schemas;
    use crate::RelationIdentity;

    #[test]
    fn relation_schema_binds_against_catalog_fixture_without_engine() {
        let column = ColumnDef {
            name: "id".into(),
            ty: ColumnType::BigInteger,
            object_id: None,
            missing_value: None,
            primary_key: true,
            not_null: true,
            not_null_explicit: false,
            not_null_name: None,
            not_null_validated: true,
            not_null_no_inherit: false,
            not_null_is_local: true,
            auto_increment: None,
            unique: false,
            default: None,
            generated: None,
            check: None,
            check_name: None,
            check_enforced: true,
            check_validated: true,
            check_no_inherit: false,
            check_is_local: true,
            check_object_id: None,
            references: None,
        };
        let catalog = crate::binding::fixture::catalog(BTreeMap::from([
            (
                RelationIdentity::new("app", "documents"),
                crate::binding::fixture::table_definition(vec![column.clone()]),
            ),
            (
                RelationIdentity::new("app", "archive"),
                crate::binding::fixture::table_definition(vec![column]),
            ),
        ]));
        let resolution =
            crate::binding::fixture::resolution(vec!["app".into()], "pg_temp_fixture".into());
        let relations = crate::ast::OperatorJoinRelations {
            left: "documents".into(),
            right: "archive".into(),
        };
        let (left, right) =
            operator_join_relation_schemas(&catalog, &resolution, Some(&relations)).unwrap();
        assert!(left.has_qualified_column("documents", "id"));
        assert!(right.has_qualified_column("archive", "id"));
        assert_eq!(left.column_type(0), Some(&ColumnType::BigInteger));
        assert_eq!(right.column_type(0), Some(&ColumnType::BigInteger));
    }
}
