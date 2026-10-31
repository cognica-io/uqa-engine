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

mod production;
pub(super) use production::operator_join_relation_schemas_with_control;

/// Bind both operator-join relations independently so each retrieval operand has its own namespace.
pub(super) fn operator_join_relation_schemas(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relations: Option<&OperatorJoinRelations>,
) -> Result<(RowSchema, RowSchema), SQLError> {
    operator_join_relation_schemas_with_control(
        catalog,
        resolution,
        relations,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
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

pub(in crate::binding) fn user_function_output_columns_with_control(
    catalog: &dyn crate::catalog::analysis::AnalysisCatalog,
    resolution: &RelationNameResolution,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<Vec<String>>>, SQLError> {
    control.check()?;
    let Some(overloads) = catalog.sql_functions(resolution, name)? else {
        return Ok(None);
    };
    for function in &overloads {
        if let Some(columns) =
            crate::semantics::user_function_output_columns_with_control(function, control)?
        {
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
            collation_oid: None,
            name: "id".into(),
            ty: ColumnType::BigInteger,
            object_id: None,
            attribute_number: None,
            missing_value: None,
            primary_key: true,
            not_null: true,
            not_null_explicit: false,
            not_null_name: None,
            not_null_identity: None,
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
            check_catalog_oid: None,
            default_catalog_oid: None,
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
