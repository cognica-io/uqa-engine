//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coerce already analyzed ROW inputs without observing their expressions a second time.

use super::{ColumnType, ExpressionType, Preparation, SQLError};
use crate::type_resolution::composite_rows::{row_cast_error, validate_field_cast, validate_width};

impl Preparation<'_> {
    pub(super) fn coerce_row_constructor(
        &mut self,
        fields: &mut [ExpressionType],
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let mut base = target;
        while let ColumnType::Domain { base: nested, .. } = base {
            base = nested;
        }
        let ColumnType::Composite(reference) = base else {
            return Ok(());
        };
        let descriptor =
            crate::expr::composites::descriptor(self.routines.composite_types(), reference.oid)?;
        for (index, attribute) in descriptor.attributes.iter().enumerate() {
            let Some(field) = fields.get_mut(index) else {
                return Err(row_cast_error(
                    target,
                    "Input has too few columns.".into(),
                    Some(self.routines),
                ));
            };
            validate_field_cast(
                target,
                field.ty.as_ref(),
                &attribute.ty,
                index + 1,
                Some(self.routines),
            )?;
            if let Some(nested) = &mut field.row_constructor {
                self.coerce_row_constructor(nested, &attribute.ty)?;
            }
            self.parameters.coerce_unknown(field, &attribute.ty)?;
        }
        validate_width(
            target,
            fields.len(),
            descriptor.attributes.len(),
            Some(self.routines),
        )
    }
}
