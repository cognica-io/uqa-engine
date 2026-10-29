//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View source names preserve virtual, view and physical lookup order without constructing unused declarations.

use super::{context, ViewRewriteContext};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn relation_columns_with_control(
    services: ViewRewriteContext<'_>,
    relation: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    control.check()?;
    if let Some(relation) = crate::catalog::VirtualRelation::from_qualified_name(relation) {
        return relation.column_names_with_control(control);
    }
    let mut output = None;
    services
        .catalog
        .with_view_definition_with_control(relation, control, &mut |definition| {
            if let Some(definition) = definition {
                let schema =
                    context::stored_view_schema_with_control(services, definition, control)?;
                let mut names = ProductionVec::new(*control);
                for (position, name) in schema.columns().iter().enumerate() {
                    names.push_produced(
                        control.copy_text(schema.public_name(position).unwrap_or(name))?,
                    )?;
                }
                output = Some(names.finish()?);
            }
            Ok(())
        })?;
    if let Some(output) = output {
        return Ok(output);
    }
    services
        .catalog
        .try_relation_column_names_with_control(relation, control)?
        .ok_or_else(|| SQLError::UnknownTable(relation.to_owned()))
}
