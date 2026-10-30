//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule metadata borrows live declarations and admits each owned destination.

use super::{EventAnalysisContext, SQLError};
use crate::{
    ast::{ColumnDef, ColumnType},
    plan::copy::{copy_syntax_with_control, SyntaxProduction},
    RowSchema,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity, RelationName,
};

enum Declaration<'a> {
    Table(&'a [ColumnDef]),
    View(&'a RowSchema),
}

impl EventAnalysisContext<'_> {
    pub fn rule_relation_column_names_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        let mut output = ProductionVec::new(*control);
        self.with_rule_declaration(name, control, &mut |declaration| {
            match declaration {
                Declaration::Table(columns) => {
                    for column in columns {
                        output.push_produced(control.copy_text(&column.name)?)?;
                    }
                }
                Declaration::View(schema) => {
                    for (index, column) in schema.columns().iter().enumerate() {
                        output.push_produced(
                            control.copy_text(schema.public_name(index).unwrap_or(column))?,
                        )?;
                    }
                }
            }
            Ok(())
        })?;
        Ok(output.finish()?)
    }

    pub fn rule_relation_definitions_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<ColumnDef>>, SQLError> {
        let mut output = ProductionVec::new(*control);
        self.with_rule_declaration(name, control, &mut |declaration| {
            match declaration {
                Declaration::Table(columns) => {
                    for column in columns {
                        output.push_produced(copy_syntax_with_control(column, control)?)?;
                    }
                }
                Declaration::View(schema) => {
                    for (index, column) in schema.columns().iter().enumerate() {
                        let syntax = SyntaxProduction::new(*control);
                        let mut column = ColumnDef::nullable(
                            syntax.text(schema.public_name(index).unwrap_or(column))?,
                            syntax.copy(schema.column_type(index).unwrap_or(&ColumnType::Text))?,
                        );
                        column.collation_oid = schema.column_collation(index);
                        output.push_produced(syntax.finish(column)?)?;
                    }
                }
            }
            Ok(())
        })?;
        Ok(output.finish()?)
    }

    fn with_rule_declaration(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(Declaration<'_>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let found = self
            .returning
            .catalog
            .with_table_row_type_with_control(name, control, &mut |columns| {
                read(Declaration::Table(columns))
            })
            .map_err(|error| match error {
                SQLError::Internal(message) => {
                    internal(format_args!("read rule columns: {message}"), control)
                }
                other => other,
            })?;
        if found {
            return Ok(());
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
                read(Declaration::View(&schema))
            })?;
        if !found {
            return Err(SQLError::UnknownTable(
                control.copy_text(name)?.into_parts().0,
            ));
        }
        Ok(())
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
