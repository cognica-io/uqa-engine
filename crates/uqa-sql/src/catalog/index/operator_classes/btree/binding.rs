//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected class retains the declaration's type while checking the class's binary input contract.

use super::BuiltinBTreeOperatorClass;
use crate::{
    catalog::type_metadata,
    type_resolution::{cast_catalog_entry_with_control, CastMethod},
    ColumnType, SQLError,
};
use uqa_core::memory::ProductionControl;

/// Checked class input compatibility, independent of comparator availability and the subtype's default class. Binding neither copies the declaration nor invokes type input or domain checks.
#[derive(Debug, Clone, Copy)]
pub struct BoundBTreeOperatorClass<'a> {
    class: &'static BuiltinBTreeOperatorClass,
    declared_type: &'a ColumnType,
}

impl BuiltinBTreeOperatorClass {
    /// Bind an explicitly selected class as `PostgreSQL`'s `IsBinaryCoercible` check does. Function and assignment-only casts do not make an input acceptable.
    pub fn bind_input<'a>(
        &'static self,
        declared_type: &'a ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<BoundBTreeOperatorClass<'a>, SQLError> {
        if !accepts_input(self.input_type_oid, declared_type, control)? {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "operator class \"{}\" does not accept data type {}",
                    self.name,
                    declared_type.display_name()
                ),
            });
        }
        Ok(BoundBTreeOperatorClass {
            class: self,
            declared_type,
        })
    }
}

impl<'a> BoundBTreeOperatorClass<'a> {
    #[must_use]
    pub const fn class(self) -> &'static BuiltinBTreeOperatorClass {
        self.class
    }

    #[must_use]
    pub const fn declared_type(self) -> &'a ColumnType {
        self.declared_type
    }
}

fn accepts_input(
    input_oid: u32,
    mut source: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    while let ColumnType::Domain { base, .. } = source {
        control.check()?;
        source = base;
    }
    if matches!(source, ColumnType::Named(_)) {
        return Err(SQLError::Internal(
            "unresolved type reached operator class input binding".into(),
        ));
    }
    if type_metadata::pg_type_oid(source) == i64::from(input_oid) {
        return Ok(true);
    }
    if let Some(generic) = type_metadata::routine_pseudo_type_name(i64::from(input_oid)) {
        return Ok(match generic {
            "anyarray" => matches!(
                source,
                ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector
            ),
            "anyenum" => matches!(source, ColumnType::Enum(_)),
            "anyrange" => matches!(source, ColumnType::Range(_) | ColumnType::DeclaredRange(_)),
            "anymultirange" => matches!(
                source,
                ColumnType::Multirange(_) | ColumnType::DeclaredMultirange(_)
            ),
            "record" => matches!(source, ColumnType::Composite(_)),
            _ => false,
        });
    }
    let Some(target) = type_metadata::builtin_scalar_type(input_oid) else {
        return Ok(false);
    };
    if matches!(target, ColumnType::Record) {
        return Ok(matches!(source, ColumnType::Composite(_)));
    }
    Ok(cast_catalog_entry_with_control(source, target, control)?
        .is_some_and(|cast| cast.context == b'i' && cast.method == CastMethod::Binary))
}

#[cfg(test)]
mod tests;
