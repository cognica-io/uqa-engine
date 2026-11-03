//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Event catalog capture preserves stored definitions and identities without invoking a routine.

use crate::{
    catalog::events::{StoredTrigger, TriggerCatalog},
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub type CapturedTriggers = Vec<(RelationIdentity, Vec<(String, StoredTrigger)>)>;

impl StoredTrigger {
    pub fn capture_definition(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        crate::plan::copy::capture_definition_with_control(self, control)
    }
}

pub fn triggers(
    values: &TriggerCatalog,
    control: &ProductionControl<'_>,
) -> Result<Produced<CapturedTriggers>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut relations = output.vector(values.len())?;
    for (relation, values) in values {
        let mut entries = output.vector(values.len())?;
        for (name, value) in values {
            entries.push((output.text(name)?, output.capture_definition(value)?))?;
        }
        relations.push((output.capture_definition(relation)?, entries.finish()?))?;
    }
    let relations = relations.finish()?;
    output.finish(relations)
}

#[cfg(test)]
mod tests;
