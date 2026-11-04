//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored type capture copies complete definitions without declaration replay or identity allocation.

use crate::{plan::copy::SyntaxProduction, SQLError};
use std::collections::BTreeMap;
use uqa_core::memory::{Produced, ProductionControl};

macro_rules! capture_types {
    ($($name:ident: $ty:ty),* $(,)?) => {$ (
        impl $ty {
            pub fn capture_definition(&self, control: &ProductionControl<'_>) -> Result<Produced<Self>, SQLError> {
                crate::plan::copy::capture_definition_with_control(self, control)
            }
        }

        pub fn $name(values: &BTreeMap<String, $ty>, control: &ProductionControl<'_>) -> Result<Produced<Vec<(String, $ty)>>, SQLError> {
            let output = SyntaxProduction::new(*control);
            let mut copied = output.vector(values.len())?;
            for (name, value) in values {
                copied.push((output.text(name)?, output.capture_definition(value)?))?;
            }
            let copied = copied.finish()?;
            output.finish(copied)
        }
    )*};
}

capture_types!(
    domains: crate::catalog::domain::StoredDomain,
    enums: crate::catalog::enum_type::StoredEnum,
    composites: crate::catalog::composite_type::StoredComposite,
    ranges: crate::catalog::range_type::StoredRange,
    shells: crate::catalog::shell_type::StoredShellType,
);

#[cfg(test)]
mod tests;
