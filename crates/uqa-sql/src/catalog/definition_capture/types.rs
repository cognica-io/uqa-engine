//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable type catalogs exclude temporary namespaces before copying complete definitions.

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
            let count = values.values().try_fold(0, |count, value| {
                output.check()?;
                Ok::<_, SQLError>(count + usize::from(!crate::catalog::temporary_namespace::is_temporary_schema_name(&value.identity.schema)))
            })?;
            let mut copied = output.vector(count)?;
            for (name, value) in values {
                output.check()?;
                if crate::catalog::temporary_namespace::is_temporary_schema_name(&value.identity.schema) {
                    continue;
                }
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
