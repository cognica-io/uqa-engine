//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View metadata borrows the retained declaration and admits only the result descriptor selected by analysis.

use super::{StoredViewKind, ViewRewriteDefinition};
use crate::{plan::QueryPlan, ColumnType, RowSchema, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

#[derive(Clone, Copy)]
pub struct ViewRewriteDefinitionRef<'a> {
    pub query: &'a QueryPlan,
    pub output_columns: Option<&'a [String]>,
    pub options: &'a [(String, String)],
    pub kind: StoredViewKind,
    pub materialized_column_types: &'a [Option<ColumnType>],
    pub materialized_column_collations: Option<&'a [u32]>,
}

impl<'a> From<&'a ViewRewriteDefinition> for ViewRewriteDefinitionRef<'a> {
    fn from(definition: &'a ViewRewriteDefinition) -> Self {
        Self {
            query: &definition.query,
            output_columns: definition.output_columns.as_deref(),
            options: &definition.options,
            kind: definition.kind,
            materialized_column_types: &definition.materialized_column_types,
            materialized_column_collations: definition.materialized_column_collations.as_deref(),
        }
    }
}

impl crate::catalog::stored_view::StoredViewDefinition {
    pub fn rewrite_definition_ref(&self) -> ViewRewriteDefinitionRef<'_> {
        ViewRewriteDefinitionRef {
            query: &self.query,
            output_columns: self.output_columns.as_deref(),
            options: &self.options,
            kind: self.kind,
            materialized_column_types: &self.materialized_column_types,
            materialized_column_collations: self.materialized_column_collations.as_deref(),
        }
    }
}

impl ViewRewriteDefinitionRef<'_> {
    pub fn row_schema_with_control(
        self,
        routines: &dyn crate::routines::RoutineResolution,
        scope: crate::binding::production::ControlledBindingContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        control.check()?;
        if self.kind == StoredViewKind::Materialized {
            return materialized_view_schema_with_control(
                self.output_columns.unwrap_or_default(),
                self.materialized_column_types,
                self.materialized_column_collations,
                control,
            );
        }
        let query_schema = {
            let _stored =
                crate::semantics::sets::placement_scope::SetPlacementScope::stored_definition();
            crate::binding::analyze_query_plan_schema_with_control(
                routines,
                self.query,
                &[],
                scope,
                None,
                control,
            )?
        };
        if let Some(columns) = self.output_columns {
            named_view_schema_with_control(&query_schema, columns, control)
        } else {
            let names = create_view_output_columns_with_control(&query_schema, &[], control)?;
            named_view_schema_with_control(&query_schema, &names, control)
        }
    }
}

pub fn create_view_output_columns_with_control(
    schema: &RowSchema,
    declared: &[String],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    control.check()?;
    if declared.len() > schema.len() {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "CREATE VIEW specifies more column names than columns".into(),
        });
    }
    let mut seen = ProductionMap::new(*control);
    let mut columns = ProductionVec::new(*control);
    for (position, column) in schema.columns().iter().enumerate() {
        control.check()?;
        let column = declared
            .get(position)
            .map(String::as_str)
            .unwrap_or_else(|| schema.public_name(position).unwrap_or(column));
        if seen.insert_copy(column, ())?.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{column}\" specified more than once"),
            });
        }
        columns.push_produced(control.copy_text(column)?)?;
    }
    Ok(columns.finish()?)
}

pub fn named_view_schema_with_control(
    query_schema: &RowSchema,
    output_columns: &[String],
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    if query_schema.len() != output_columns.len() {
        return Err(SQLError::Internal(format!(
            "stored view row type has {} columns but its query has {}",
            output_columns.len(),
            query_schema.len()
        )));
    }
    RowSchema::with_output_metadata_with_control(
        None,
        output_columns.iter().enumerate().map(|(position, name)| {
            (
                name.as_str(),
                query_schema.column_type(position),
                query_schema.column_collation(position),
                query_schema.record_metadata(position).cloned(),
            )
        }),
        false,
        control,
    )
}

pub fn materialized_view_schema_with_control(
    columns: &[String],
    types: &[Option<ColumnType>],
    collations: Option<&[u32]>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let collations = super::materialized::validate_metadata(columns, types, collations, control)?;
    RowSchema::with_column_metadata_with_control(
        None,
        columns
            .iter()
            .zip(types)
            .zip(collations)
            .map(|((name, ty), oid)| (name.as_str(), ty.as_ref(), Some(*oid))),
        control,
    )
}
