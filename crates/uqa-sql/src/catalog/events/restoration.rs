//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the bound function identity before restoring stored trigger metadata.
use super::definition::EventAnalysisContext;
use crate::{ast::CreateTrigger, catalog::resolution::RelationLookupMode};

pub fn trigger_function_object_id(
    context: &EventAnalysisContext<'_>,
    definition: &CreateTrigger,
) -> Result<[u8; 16], String> {
    context
        .resolve_trigger_function(&definition.function, RelationLookupMode::Bound)
        .map_err(|error| format!("restore trigger function identity: {error}"))?
        .def
        .object_id
        .ok_or_else(|| {
            format!(
                "restore trigger catalog: function `{}` has no object identity",
                definition.function
            )
        })
}
