//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native type registries share SQL's immutable named-definition owner.

use super::named_definitions::{sealed, StoredDefinition};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

pub use super::named_definitions::{
    CapturedNamedDefinitions as CapturedTypeDefinitions, NamedDefinitionIter as TypeDefinitionIter,
    NamedDefinitionView as TypeDefinitionView, NamedDefinitions as TypeDefinitions,
};

/// Stored type forms whose definitions and temporary classification belong to SQL.
pub trait StoredTypeDefinition: StoredDefinition {}

macro_rules! definitions {
    ($($ty:ty),+ $(,)?) => {$ (
        impl sealed::Definition for $ty {}
        impl StoredTypeDefinition for $ty {}
        impl StoredDefinition for $ty {
            fn is_temporary(&self, _name: &str) -> bool {
                super::temporary_namespace::is_temporary_schema_name(&self.identity.schema)
            }
            fn copy_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
                self.capture_definition(control)
            }
        }
    )+};
}

definitions!(
    super::domain::StoredDomain,
    super::enum_type::StoredEnum,
    super::composite_type::StoredComposite,
    super::range_type::StoredRange,
    super::shell_type::StoredShellType,
);

#[cfg(test)]
mod tests;
