//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Inline and retained type copies share each node, name and lease transition.

use super::{ColumnType, Produced, ProductionControl, ValueRetentionError};
use crate::ast::{CompositeTypeReference, EnumTypeReference, RangeTypeReference};
use std::future::Future;
use uqa_core::image_io::AsyncImageInput;

pub(super) trait Work: Send {
    type Error: From<ValueRetentionError>;
    fn checkpoint(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>, Self::Error>> + Send;
}

impl Work for () {
    type Error = ValueRetentionError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        control.copy_text(source)
    }
}

impl Work for AsyncImageInput<'_> {
    type Error = crate::SQLError;
    async fn checkpoint(&mut self) -> Result<(), Self::Error> {
        Ok(AsyncImageInput::checkpoint(self).await?)
    }
    async fn text(
        &mut self,
        source: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, Self::Error> {
        crate::catalog::work::Work::text(self, source, control).await
    }
}

pub(super) async fn copy<W: Work>(
    mut source: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<ColumnType>, W::Error> {
    // The incomplete type retires before its accumulated payload lease on every exit.
    let mut memory = control.empty_reservation();
    let mut value = ColumnType::Void;
    let mut target = &mut value;
    loop {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        let (node, node_memory) = node(source, control, work).await?.into_parts();
        memory = control.combine(memory, node_memory);
        *target = node;
        match (source, target) {
            (ColumnType::Array(child), ColumnType::Array(next))
            | (ColumnType::Domain { base: child, .. }, ColumnType::Domain { base: next, .. }) => {
                source = child;
                target = next;
            }
            _ => return Ok(control.finish(value, memory)?),
        }
    }
}

async fn node<W: Work>(
    source: &ColumnType,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<ColumnType>, W::Error> {
    macro_rules! reference {
        ($reference:expr, $ty:ident, $variant:ident, $($field:ident),+ $(,)?) => {{
            let schema = work.text(&$reference.schema, control).await?;
            let name = work.text(&$reference.name, control).await?;
            let (schema, schema_memory) = schema.into_parts();
            let (name, name_memory) = name.into_parts();
            control.finish(
                ColumnType::$variant($ty { schema, name, $($field: $reference.$field,)+ }),
                control.combine(schema_memory, name_memory),
            )?
        }};
    }
    Ok(match source {
        ColumnType::Named(name) => {
            let (name, memory) = work.text(name, control).await?.into_parts();
            control.finish(ColumnType::Named(name), memory)?
        }
        ColumnType::Array(_) => ColumnType::array_with_control(
            control.finish(ColumnType::Void, control.empty_reservation())?,
            control,
        )?,
        ColumnType::Enum(value) => reference!(value, EnumTypeReference, Enum, oid, array_oid),
        ColumnType::Composite(value) => {
            reference!(
                value,
                CompositeTypeReference,
                Composite,
                oid,
                array_oid,
                relation_oid
            )
        }
        ColumnType::DeclaredRange(value) => {
            reference!(
                value,
                RangeTypeReference,
                DeclaredRange,
                oid,
                array_oid,
                alignment
            )
        }
        ColumnType::DeclaredMultirange(value) => {
            reference!(
                value,
                RangeTypeReference,
                DeclaredMultirange,
                oid,
                array_oid,
                alignment
            )
        }
        ColumnType::Domain {
            schema,
            name,
            oid,
            array_oid,
            collation_oid,
            ..
        } => {
            let schema = work.text(schema, control).await?;
            let name = work.text(name, control).await?;
            let box_memory = control.reserve(size_of::<ColumnType>())?;
            let (schema, schema_memory) = schema.into_parts();
            let (name, name_memory) = name.into_parts();
            control.finish(
                ColumnType::Domain {
                    schema,
                    name,
                    oid: *oid,
                    array_oid: *array_oid,
                    collation_oid: *collation_oid,
                    base: Box::new(ColumnType::Void),
                },
                control.combine(control.combine(schema_memory, name_memory), box_memory),
            )?
        }
        scalar => scalar.clone_scalar_with_control(control)?,
    })
}

#[cfg(test)]
mod tests;
