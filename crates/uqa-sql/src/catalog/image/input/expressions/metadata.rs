//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated literals keep original type and tuple metadata without running input or casts.

use super::super::collections::ImageValue;
use super::{boxed, payload, Expr, Fields, MemoryReservation, SQLError};
use crate::{
    ast::{CompositeRowBinding, CompositeRowLayout, RetainedInputLiteral},
    expr::composites::{
        CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor,
        DroppedCompositeAttribute,
    },
    ir::RetainedScalarLiteral,
};
use uqa_core::memory::{Budgeted, Produced, ProductionControl};

impl Fields<'_> {
    pub(in super::super) async fn scalar_row_binding(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeRowBinding, SQLError> {
        let ty = payload(self.binary.text().await?, memory);
        let attributes = self.binary.count().await?;
        let layout = if self.flag().await? {
            Some(CompositeRowLayout {
                width: i16::from_le_bytes(self.binary.array().await?),
                implicit: self.flag().await?,
            })
        } else {
            None
        };
        let type_count = if self.flag().await? {
            Some(self.binary.count().await?)
        } else {
            None
        };
        let attributes = payload(self.image_values(attributes).await?, memory);
        let argument_types = if let Some(count) = type_count {
            Some(payload(self.image_values(count).await?, memory))
        } else {
            None
        };
        Ok(CompositeRowBinding {
            ty,
            attributes,
            layout,
            argument_types,
        })
    }

    pub(in super::super) async fn composite_source(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeConstantSource, SQLError> {
        let count = self.binary.count().await?;
        let value = payload(self.scalar_value().await?, memory);
        let descriptors = payload(self.image_values(count).await?, memory);
        Ok(CompositeConstantSource { value, descriptors })
    }

    async fn scalar_descriptor(
        &mut self,
        memory: &mut MemoryReservation,
    ) -> Result<CompositeTypeDescriptor, SQLError> {
        let type_oid = u32::from_le_bytes(self.binary.array().await?);
        let relation_oid = u32::from_le_bytes(self.binary.array().await?);
        let attributes = self.binary.count().await?;
        let dropped = self.binary.count().await?;
        let attributes = payload(self.image_values(attributes).await?, memory);
        let dropped = payload(self.image_values(dropped).await?, memory);
        Ok(CompositeTypeDescriptor {
            dropped,
            type_oid,
            relation_oid,
            attributes,
        })
    }

    pub(super) async fn retained_literal(
        &mut self,
        memory: &mut MemoryReservation,
        value: &mut Expr,
    ) -> Result<(), SQLError> {
        let literal = self.retained_scalar_data().await?;
        *value = Expr::RetainedLiteral(boxed(RetainedInputLiteral(literal), memory)?);
        Ok(())
    }

    pub(in super::super) async fn retained_scalar_data(
        &mut self,
    ) -> Result<RetainedScalarLiteral, SQLError> {
        let (declaration, declaration_memory) = self.optional_text().await?.into_parts();
        let declaration =
            declaration.map(|name| Produced::from(Budgeted::new(name, declaration_memory)));
        let collation_oid = self.optional_number().await?;
        let ty = if self.flag().await? {
            Some(Produced::from(self.column_type().await?))
        } else {
            None
        };
        let value = self.scalar_value().await?;
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
        *self.retained_children = self
            .retained_children
            .checked_add(bytes)
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        Ok(literal)
    }
}

impl ImageValue for CompositeTypeDescriptor {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let value = input.scalar_descriptor(&mut memory).await?;
        Ok(Budgeted::new(value, memory))
    }
}

impl ImageValue for CompositeAttribute {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        let mut memory = input.binary.memory.empty_reservation();
        let name = payload(input.binary.text().await?, &mut memory);
        let collation_oid = input.optional_number().await?;
        let number = i16::from_le_bytes(input.binary.array().await?);
        let ty = payload(input.column_type().await?, &mut memory);
        Ok(Budgeted::new(
            Self {
                name,
                ty,
                collation_oid,
                number,
            },
            memory,
        ))
    }
}

impl ImageValue for DroppedCompositeAttribute {
    async fn read(input: &mut Fields<'_>) -> Result<Budgeted<Self>, SQLError> {
        Ok(Budgeted::new(
            Self {
                number: i16::from_le_bytes(input.binary.array().await?),
                length: i64::from_le_bytes(input.binary.array().await?),
                alignment: input.binary.array::<1>().await?[0],
            },
            input.binary.memory.empty_reservation(),
        ))
    }
}
