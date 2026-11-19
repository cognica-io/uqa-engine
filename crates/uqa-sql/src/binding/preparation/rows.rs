//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coerce already analyzed ROW inputs without observing their expressions a second time.

use crate::binding::preparation::{LiteralInput, SchemaContext};

use super::{ColumnType, ExpressionType, Preparation, SQLError};
use crate::type_resolution::composite_rows::{row_cast_error, validate_width};

impl<I: LiteralInput, C: SchemaContext> Preparation<'_, I, C> {
    pub(super) fn coerce_row_comparison(
        &mut self,
        op: crate::ast::BinaryOp,
        left: &mut ExpressionType,
        right: &mut ExpressionType,
        distinct: bool,
    ) -> Result<(), SQLError> {
        let (Some(left), Some(right)) = (&mut left.row_constructor, &mut right.row_constructor)
        else {
            return Err(SQLError::Internal(
                "ROW analysis lost its input fields".into(),
            ));
        };
        crate::type_resolution::row_comparison::validate_width(left.len(), right.len(), distinct)?;
        let mut interpretations = Vec::new();
        let mut operator_name = "";
        let ordered = !distinct && left.len() > 1;
        for (left, right) in left.iter_mut().zip(right) {
            let [left, right, result] = self.binary_types(op, left, right)?;
            crate::type_resolution::row_comparison::require_boolean(&result)?;
            if ordered {
                let operator =
                    crate::type_resolution::binary_operator_catalog_entry(op, [&left, &right])?;
                operator_name = operator.name;
                interpretations.push(operator.row_comparison_interpretations().collect());
            }
        }
        if ordered {
            crate::type_resolution::row_comparison::interpretation(
                operator_name,
                &interpretations,
            )?;
        }
        Ok(())
    }

    pub(super) async fn coerce_row_constructor<W: crate::catalog::work::Work + Send>(
        &mut self,
        fields: &mut [ExpressionType],
        target: &ColumnType,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let mut input = crate::catalog::work::ReferenceWork {
            control: &control,
            work,
        };
        let mut base = target;
        while let ColumnType::Domain { base: nested, .. } = base {
            input.checkpoint().await?;
            base = nested;
        }
        let ColumnType::Composite(reference) = base else {
            return Ok(());
        };
        let descriptor = crate::expr::composites::descriptor_with_control(
            self.routines.composite_types(),
            reference.oid,
            &self.scope.control(),
        )?;
        for (index, attribute) in descriptor.attributes.iter().enumerate() {
            input.checkpoint().await?;
            let Some(field) = fields.get_mut(index) else {
                return Err(row_cast_error(
                    target,
                    "Input has too few columns.".into(),
                    Some(self.routines),
                ));
            };
            crate::type_resolution::composite_rows::validate_field_cast_with_control(
                target,
                field.ty.as_deref(),
                &attribute.ty,
                index + 1,
                self.routines,
                &self.scope.control(),
            )?;
            if let Some(nested) = &mut field.row_constructor {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    control,
                    self.coerce_row_constructor(nested, &attribute.ty, input.work)
                )?;
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
