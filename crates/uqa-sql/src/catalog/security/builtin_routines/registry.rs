//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable built-in ACL tuples retain their original revision and role endpoints.

use super::BuiltinRoutineSecurity;
use crate::{
    catalog::named_definitions::{
        sealed, CapturedDefinitions, DefinitionKey, DefinitionRegistry, StoredDefinition,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl};

pub type BuiltinRoutineSecurities = DefinitionRegistry<u32, BuiltinRoutineSecurity>;
pub type CapturedBuiltinRoutineSecurities = CapturedDefinitions<u32, BuiltinRoutineSecurity>;

impl sealed::Key for u32 {}
impl DefinitionKey for u32 {
    type View = Self;
    fn payload_bytes(&self) -> usize {
        0
    }
}

impl sealed::Definition for BuiltinRoutineSecurity {}
impl StoredDefinition<u32> for BuiltinRoutineSecurity {
    fn is_temporary(&self, _: &u32) -> bool {
        false
    }
    fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
        self.capture_security(control)
    }
}
