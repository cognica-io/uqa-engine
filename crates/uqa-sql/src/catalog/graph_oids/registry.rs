//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native graph catalog identities retain complete per-graph records in immutable roots.

use super::GraphCatalogOids;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedNamedDefinitions, NamedDefinitions, StoredDefinition,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type GraphOidRegistry = NamedDefinitions<GraphCatalogOids>;
pub type CapturedGraphOidRegistry = CapturedNamedDefinitions<GraphCatalogOids>;

impl sealed::Definition for GraphCatalogOids {}
impl StoredDefinition for GraphCatalogOids {
    fn is_temporary(&self, _: &str) -> bool {
        false
    }

    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        let Self {
            namespace,
            label_sequence,
            labels,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            namespace: *namespace,
            label_sequence: *label_sequence,
            labels: output.capture_definition(labels)?,
        };
        output.finish(copied)
    }
}

#[cfg(test)]
mod tests;
