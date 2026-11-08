//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated literals keep original type and tuple metadata without running input or casts.

use super::{boxed, payload, Expr, Input, MemoryReservation, SQLError};
use crate::{
    ast::{CompositeRowBinding, CompositeRowLayout, RetainedInputLiteral},
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
    ir::RetainedScalarLiteral,
};
use uqa_core::memory::{Budgeted, Produced, ProductionControl};

impl Input<'_> {
    pub(in super::super) fn scalar_row_binding(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeRowBinding, SQLError> {
        let ty = payload(self.binary.text()?, memory);
        let attributes = self.binary.count()?;
        let layout = if self.flag()? {
            Some(CompositeRowLayout {
                width: i16::from_le_bytes(self.binary.array()?),
                implicit: self.flag()?,
            })
        } else {
            None
        };
        let type_count = if self.flag()? {
            Some(self.binary.count()?)
        } else {
            None
        };
        let attributes = self.items(attributes, memory, |input, _| {
            Ok(i16::from_le_bytes(input.binary.array()?))
        })?;
        let argument_types = type_count
            .map(|count| {
                self.items(count, memory, |input, memory| {
                    Ok(payload(input.column_type()?, memory))
                })
            })
            .transpose()?;
        Ok(CompositeRowBinding {
            ty,
            attributes,
            layout,
            argument_types,
        })
    }

    pub(in super::super) fn composite_source(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeConstantSource, SQLError> {
        let count = self.binary.count()?;
        let value = payload(self.scalar_value()?, memory);
        let descriptors = self.items(count, memory, Self::scalar_descriptor)?;
        Ok(CompositeConstantSource { value, descriptors })
    }

    fn scalar_descriptor(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeTypeDescriptor, SQLError> {
        let type_oid = u32::from_le_bytes(self.binary.array()?);
        let relation_oid = u32::from_le_bytes(self.binary.array()?);
        let attributes = self.binary.count()?;
        let dropped = self.binary.count()?;
        let attributes = self.items(attributes, memory, |input, memory| {
            let name = payload(input.binary.text()?, memory);
            let collation_oid = input.optional_number()?;
            let number = i16::from_le_bytes(input.binary.array()?);
            let ty = payload(input.column_type()?, memory);
            Ok(CompositeAttribute {
                name,
                ty,
                collation_oid,
                number,
            })
        })?;
        let dropped = self.items(dropped, memory, |input, _| {
            Ok(DroppedCompositeAttribute {
                number: i16::from_le_bytes(input.binary.array()?),
                length: i64::from_le_bytes(input.binary.array()?),
                alignment: input.binary.array::<1>()?[0],
            })
        })?;
        Ok(CompositeTypeDescriptor {
            dropped,
            type_oid,
            relation_oid,
            attributes,
        })
    }

    pub(super) fn retained_literal(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<Expr, SQLError> {
        let literal = self.retained_scalar_data()?;
        Ok(Expr::RetainedLiteral(boxed(
            RetainedInputLiteral(literal),
            memory,
        )?))
    }

    pub(in super::super) fn retained_scalar_data(
        &mut self,
    ) -> Result<RetainedScalarLiteral, SQLError> {
        let (declaration, declaration_memory) = self.optional_text()?.into_parts();
        let declaration =
            declaration.map(|name| Produced::from(Budgeted::new(name, declaration_memory)));
        let collation_oid = self.optional_number()?;
        let ty = if self.flag()? {
            Some(Produced::from(self.column_type()?))
        } else {
            None
        };
        let value = self.scalar_value()?;
        let control = ProductionControl::new(
            self.binary.memory,
            self.binary.cancellation,
            self.binary.cancellation,
        );
        let (literal, bytes) = RetainedScalarLiteral::from_produced(
            value.into(),
            ty,
            declaration,
            collation_oid,
            &control,
        )?;
        self.retained_children = self
            .retained_children
            .checked_add(bytes)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        Ok(literal)
    }
}
