//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached catalog definitions reuse the complete syntax producer without parsing or rebinding.

use crate::{plan::copy::SyntaxProduction, SQLError};
use uqa_core::memory::{Produced, ProductionControl};

pub mod events;
pub mod foreign;
pub mod identities;
pub mod types;
pub mod views;

macro_rules! capture_list {
    ($($name:ident: $ty:ty),* $(,)?) => {$ (
        pub fn $name(values: &[$ty], control: &ProductionControl<'_>) -> Result<Produced<Vec<$ty>>, SQLError> {
            let output = SyntaxProduction::new(*control);
            let mut copied = output.vector(values.len())?;
            for value in values {
                copied.push(output.capture_definition(value)?)?;
            }
            let copied = copied.finish()?;
            output.finish(copied)
        }
    )*};
}

capture_list!(
    columns: crate::ast::ColumnDef,
    checks: crate::ast::TableCheck,
    foreign_keys: crate::ast::ForeignKey,
    keys: crate::ast::TableKeyConstraint,
    dropped_attributes: crate::catalog::relation_attributes::DroppedAttribute,
    names: String,
);

pub fn hierarchy(
    value: &crate::ast::TableHierarchy,
    control: &ProductionControl<'_>,
) -> Result<Produced<crate::ast::TableHierarchy>, SQLError> {
    crate::plan::copy::capture_definition_with_control(value, control)
}

pub fn optional_name(
    value: &Option<String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<String>>, SQLError> {
    crate::plan::copy::copy_syntax_with_control(value, control)
}

#[cfg(test)]
mod tests;
