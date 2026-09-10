//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{RowSchema, SQLError};

pub fn create_view_output_columns(
    schema: &RowSchema,
    declared: &[String],
) -> Result<Vec<String>, SQLError> {
    if declared.len() > schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "CREATE VIEW specifies more column names than columns".into(),
        });
    }
    let columns = schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, column)| {
            declared
                .get(position)
                .cloned()
                .unwrap_or_else(|| schema.public_name(position).unwrap_or(column).to_string())
        })
        .collect::<Vec<_>>();
    let mut seen = std::collections::BTreeSet::new();
    for column in &columns {
        if !seen.insert(column) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
    }
    Ok(columns)
}

pub fn named_view_schema(
    query_schema: &RowSchema,
    output_columns: &[String],
) -> Result<RowSchema, SQLError> {
    if query_schema.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "stored view row type has {} columns but its query has {}",
            output_columns.len(),
            query_schema.len()
        )));
    }
    Ok(RowSchema::with_types(
        output_columns.to_vec(),
        query_schema.column_types().to_vec(),
    ))
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
}

impl ViewRewriteDefinition {
    pub fn row_schema(
        &self,
        routines: &dyn crate::routines::RoutineResolution,
        catalog: super::analysis::CatalogReadView,
        resolution: super::resolution::RelationNameResolution,
    ) -> Result<crate::RowSchema, crate::SQLError> {
        use crate::catalog::view::{create_view_output_columns, named_view_schema};
        use crate::SQLError;
        if self.kind == StoredViewKind::Materialized {
            let output_columns = self.output_columns.clone().unwrap_or_default();
            if output_columns.len() != self.materialized_column_types.len() {
                return Err(SQLError::Internal(
                    "stored materialized view column metadata is inconsistent".into(),
                ));
            }
            return Ok(crate::RowSchema::with_types(
                output_columns,
                self.materialized_column_types.clone(),
            ));
        }
        let query_schema = crate::binding::analyze_query_plan_schema_with_catalog(
            routines,
            &self.query,
            &[],
            catalog,
            resolution,
        )?;
        let output_columns = match &self.output_columns {
            Some(columns) => columns.clone(),
            None => create_view_output_columns(&query_schema, &[])?,
        };
        named_view_schema(&query_schema, &output_columns)
    }
}
