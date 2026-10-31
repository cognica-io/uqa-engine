//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dynamic type components keep their quoting and namespace workspace inside the caller's allowance.

use super::{ColumnType, TypeName};
use uqa_core::{
    memory::{Produced, ProductionControl},
    ValueRetentionError,
};

#[cfg(test)]
mod tests;

pub(super) fn name(
    ty: &ColumnType,
    regtype: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    control.check()?;
    let reference = match ty {
        ColumnType::Enum(reference) => Some((&reference.schema, &reference.name)),
        ColumnType::Composite(reference) => Some((&reference.schema, &reference.name)),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            Some((&reference.schema, &reference.name))
        }
        ColumnType::Domain { schema, name, .. } if regtype => Some((schema, name)),
        ColumnType::Array(element) => {
            let element = name(element, regtype, control)?;
            return control.format(format_args!("{}[]", element.as_str()));
        }
        _ => None,
    };
    if let Some((schema, name)) = reference {
        return if regtype {
            super::super::display::visible_type_name_with_control(schema, name, control)
        } else {
            crate::expr::quote_ident_with_control(name, control)
        };
    }
    control.format(format_args!("{}", TypeName { ty, regtype }))
}
