//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{RowSchema, SQLError};

mod production;
pub use production::{
    create_view_output_columns_with_control, materialized_view_schema_with_control,
    named_view_schema_with_control, ViewRewriteDefinitionRef,
};
mod materialized;
pub use materialized::materialized_view_schema;

/// Scanning an unpopulated materialized view fails even if the query would read no rows.
pub fn require_materialized_view_population(name: &str, populated: bool) -> Result<(), SQLError> {
    if populated {
        return Ok(());
    }
    let (_, relation) =
        uqa_core::RelationIdentity::parse_reference(name).map_err(SQLError::Unsupported)?;
    Err(SQLError::Diagnostic {
        sqlstate: "55000".into(),
        message: format!("materialized view \"{relation}\" has not been populated"),
        detail: None,
        hint: Some("Use the REFRESH MATERIALIZED VIEW command.".into()),
    })
}

pub fn create_view_output_columns(
    schema: &RowSchema,
    declared: &[String],
) -> Result<Vec<String>, SQLError> {
    create_view_output_columns_with_control(
        schema,
        declared,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|output| {
        output
            .into_uncontrolled()
            .expect("ordinary view column names")
    })
}

pub fn named_view_schema(
    query_schema: &RowSchema,
    output_columns: &[String],
) -> Result<RowSchema, SQLError> {
    named_view_schema_with_control(
        query_schema,
        output_columns,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn validate_view_column_types(
    query_schema: &RowSchema,
    output_columns: &[String],
) -> Result<(), SQLError> {
    for (position, column) in output_columns.iter().enumerate() {
        if let Some(ty) = query_schema.column_type(position) {
            crate::schema::columns::validate_postgres_relation_column_type(column, ty)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StoredViewKind {
    #[default]
    View,
    Materialized,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ViewMutationCapabilities {
    pub insertable: bool,
    pub updatable: bool,
    pub deletable: bool,
}
impl ViewMutationCapabilities {
    pub const fn fully_updatable(self) -> bool {
        self.updatable && self.deletable
    }
}

#[derive(Clone)]
pub struct ViewRewriteDefinition {
    pub query: crate::plan::QueryPlan,
    pub output_columns: Option<Vec<String>>,
    pub options: Vec<(String, String)>,
    pub kind: StoredViewKind,
    pub materialized_column_types: Vec<Option<crate::ColumnType>>,
    pub materialized_column_collations: Option<Vec<u32>>,
}

impl ViewRewriteDefinition {
    pub fn row_schema(
        &self,
        routines: &dyn crate::routines::RoutineResolution,
        catalog: super::analysis::CatalogReadView,
        resolution: super::resolution::RelationNameResolution,
    ) -> Result<crate::RowSchema, crate::SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        let scope = crate::binding::production::ControlledBindingContext {
            catalog,
            resolution: resolution.into(),
            ctes: crate::binding::production::BindingMap::new(),
            deferred_ctes: crate::binding::production::BindingMap::new(),
            non_returning_ctes: crate::binding::production::BindingNameSet::new(),
            scalar_subqueries: &[],
        };
        ViewRewriteDefinitionRef::from(self).row_schema_with_control(routines, scope, &control)
    }
}

#[cfg(test)]
mod tests;
