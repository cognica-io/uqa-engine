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
use std::collections::BTreeMap;
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedForeignServer {
    pub name: String,
    pub fdw_type: String,
    pub options: Vec<(String, String)>,
    pub metadata: ForeignServerMetadata,
}

pub fn options(
    values: &BTreeMap<String, String>,
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
    values: &BTreeMap<String, ForeignServerDefinition>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, CapturedForeignServer)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut entries = output.vector(values.len())?;
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
                options: output.take(options(values, control)?),
                metadata: output.capture_definition(metadata)?,
            },
        ))?;
    }
    let entries = entries.finish()?;
    output.finish(entries)
}

#[cfg(test)]
mod tests;
