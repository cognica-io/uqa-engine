//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign descriptors retain bound identities and ordered options without invoking a handler.

use crate::{
    catalog::{
        foreign_server::{ForeignServerDefinition, ForeignServerMetadata},
        foreign_wrapper::{ForeignWrapperDefinition, ForeignWrappers},
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

/// Preserve native columns and return their separately retained child footprint for independent catalog admission.
pub fn stored_columns(
    values: &Vec<crate::ast::ColumnDef>,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Vec<crate::ast::ColumnDef>>, usize), SQLError> {
    crate::plan::copy::capture_stored_with_footprint(values, control)
}

/// Preserve native checks and return their separately retained child footprint.
pub fn stored_checks(
    values: &Vec<crate::ast::TableCheck>,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Vec<crate::ast::TableCheck>>, usize), SQLError> {
    crate::plan::copy::capture_stored_with_footprint(values, control)
}

pub fn stored_options(
    values: &uqa_core::memory::OwnedMap<String, String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<uqa_core::memory::OwnedMap<String, String>>, SQLError> {
    crate::plan::copy::capture_stored_with_control(values, control)
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedForeignServer {
    pub name: String,
    pub fdw_type: String,
    pub options: Vec<(String, String)>,
    pub metadata: ForeignServerMetadata,
}

pub fn options<'a>(
    values: impl ExactSizeIterator<Item = (&'a String, &'a String)>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, String)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
    for (name, value) in values {
        entries.push((output.text(name)?, output.text(value)?))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

pub fn wrappers(
    values: &ForeignWrappers,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, ForeignWrapperDefinition)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
    for (name, value) in values {
        entries.push((output.text(name)?, output.capture_definition(value)?))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

pub fn servers(
    values: &impl crate::catalog::named_definitions::NamedDefinitionView<ForeignServerDefinition>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, CapturedForeignServer)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let values = values.iter();
    let mut entries = output.vector(values.size_hint().0)?;
    for (key, value) in values {
        let ForeignServerDefinition {
            name,
            fdw_type,
            options: values,
            metadata,
        } = value;
        entries.push((
            output.text(key)?,
            CapturedForeignServer {
                name: output.text(name)?,
                fdw_type: output.text(fdw_type)?,
                options: output.take(options(values.iter(), control)?),
                metadata: output.capture_definition(metadata)?,
            },
        ))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

#[cfg(test)]
mod tests;
