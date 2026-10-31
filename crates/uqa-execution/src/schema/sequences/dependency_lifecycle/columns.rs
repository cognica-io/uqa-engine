//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish names after validating the same stored default and generated sequence references as a table description.

use super::SequenceDependencyContext;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{ast::ColumnDef, SQLError};

impl SequenceDependencyContext<'_> {
    pub fn relation_column_names_with_control(
        &self,
        columns: &[ColumnDef],
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        self.observe_column_sequence_references_with_control(columns, control)?;
        let mut names = ProductionVec::new(*control);
        for column in columns {
            names.push_produced(control.copy_text(&column.name)?)?;
        }
        Ok(names.finish()?)
    }

    /// Preserve default-then-generated dependency observation without materializing replacement expression trees.
    pub fn observe_column_sequence_references_with_control(
        &self,
        columns: &[ColumnDef],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        for column in columns {
            control.check()?;
            for expression in column.default.iter().chain(
                column
                    .generated
                    .iter()
                    .map(|generated| generated.expression.as_ref()),
            ) {
                let mut refreshed = false;
                uqa_sql::schema::dependencies::rewrites::visit_sequence_function_references_with_control(expression, control, &mut |reference| {
                    if !refreshed {
                        self.sequences.refresh_sequences().map_err(|error| SQLError::Internal(error.to_string()))?;
                        refreshed = true;
                    }
                    self.expressions.stored_sequence_name_with_control(reference, control)?;
                    Ok(())
                })?;
            }
        }
        Ok(())
    }
}
