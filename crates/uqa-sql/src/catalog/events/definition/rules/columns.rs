//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule target names borrow live declarations and retain only the admitted output.

use super::{EventAnalysisContext, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};

impl EventAnalysisContext<'_> {
    pub fn rule_relation_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        control.check()?;
        let mut output = ProductionVec::new(*control);
        let found = self
            .returning
            .catalog
            .with_table_row_type_with_control(name, control, &mut |columns| {
                for column in columns {
                    output.push_produced(control.copy_text(&column.name)?)?;
                }
                Ok(())
            })
            .map_err(|error| match error {
                SQLError::Internal(message) => {
                    internal(format_args!("read rule columns: {message}"), control)
                }
                other => other,
            })?;
        if found {
            return Ok(output.finish()?);
        }
        let reference = RelationIdentity::try_borrowed_reference(name).map_err(|error| {
            internal(
                format_args!("decode rule relation `{name}`: {error}"),
                control,
            )
        })?;
        let relation = reference.identity_with_control(RelationName::decoded("public"), control)?;
        let found = self
            .catalog
            .with_view_with_control(&relation, control, &mut |view| {
                let schema = self
                    .catalog
                    .stored_view_schema_with_control(view, control)?;
                for (index, column) in schema.columns().iter().enumerate() {
                    output.push_produced(
                        control.copy_text(schema.public_name(index).unwrap_or(column))?,
                    )?;
                }
                Ok(())
            })?;
        if !found {
            return Err(SQLError::UnknownTable(
                control.copy_text(name)?.into_parts().0,
            ));
        }
        Ok(output.finish()?)
    }
}

fn internal(message: std::fmt::Arguments<'_>, control: &ProductionControl<'_>) -> SQLError {
    match control.format(message) {
        Ok(message) => SQLError::Internal(message.into_parts().0),
        Err(error) => error.into(),
    }
}

#[cfg(test)]
mod tests;
