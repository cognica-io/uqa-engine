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
    let (ty, dimensions) =
        crate::catalog::work::inline(ty.array_shape_with_work(control, &mut ()))?;
    let reference = match ty {
        ColumnType::Enum(reference) => Some((&reference.schema, &reference.name)),
        ColumnType::Composite(reference) => Some((&reference.schema, &reference.name)),
        ColumnType::DeclaredRange(reference) | ColumnType::DeclaredMultirange(reference) => {
            Some((&reference.schema, &reference.name))
        }
        ColumnType::Domain { schema, name, .. } if regtype => Some((schema, name)),
        _ => None,
    };
    let scalar = if let Some((schema, name)) = reference {
        if regtype {
            super::super::display::visible_type_name_with_control(schema, name, control)
        } else {
            crate::expr::quote_ident_with_control(name, control)
        }
    } else {
        control.format(format_args!("{}", TypeName { ty, regtype }))
    }?;
    crate::catalog::work::inline(ColumnType::array_name_with_work(
        scalar,
        dimensions,
        control,
        &mut (),
    ))
}
