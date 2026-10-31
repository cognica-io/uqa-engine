//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine matching admits mutable scratch independently from immutable candidate fields and diagnostics.

use super::{RoutineDefaultType, RoutinePolymorphicType, RoutineSignatureMatchError};
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl},
    ValueRetentionError,
};

mod mapping;
mod matching;
mod output;
mod polymorphic;

pub(crate) use matching::match_routine_candidate_with_control;
pub(crate) use output::RoutineInvocationParts;

#[derive(Debug)]
pub(crate) enum RoutineSignatureProductionError {
    Signature(Produced<RoutineSignatureMatchError>),
    Production(SQLError),
}

impl From<ValueRetentionError> for RoutineSignatureProductionError {
    fn from(error: ValueRetentionError) -> Self {
        Self::Production(error.into())
    }
}
impl From<SQLError> for RoutineSignatureProductionError {
    fn from(error: SQLError) -> Self {
        Self::Production(error)
    }
}

type Result<T> = std::result::Result<T, RoutineSignatureProductionError>;

struct OwnedIterator<T> {
    values: std::vec::IntoIter<T>,
    _memory: Option<MemoryReservation>,
}
impl<T> Iterator for OwnedIterator<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl<T> ExactSizeIterator for OwnedIterator<T> {}
fn owned_iterator<T>(values: Produced<Vec<T>>) -> OwnedIterator<T> {
    let (values, memory) = values.into_parts();
    OwnedIterator {
        values: values.into_iter(),
        _memory: memory,
    }
}

/// This constructor only combines completed fields; field replacement stays with each independent producer above it.
struct Components<'a> {
    control: ProductionControl<'a>,
    memory: Option<MemoryReservation>,
}

impl<'a> Components<'a> {
    fn new(control: ProductionControl<'a>) -> Self {
        Self {
            control,
            memory: control.empty_reservation(),
        }
    }
    fn take<T>(&mut self, value: Produced<T>) -> T {
        let (value, memory) = value.into_parts();
        self.memory = self.control.combine(self.memory.take(), memory);
        value
    }
    fn text(&mut self, value: &str) -> std::result::Result<String, SQLError> {
        let value = self.control.copy_text(value)?;
        Ok(self.take(value))
    }
    fn ty(&mut self, value: &ColumnType) -> std::result::Result<ColumnType, SQLError> {
        let value = value.clone_with_control(&self.control)?;
        Ok(self.take(value))
    }
    fn default_type(
        &mut self,
        value: &RoutineDefaultType,
    ) -> std::result::Result<RoutineDefaultType, SQLError> {
        Ok(match value {
            RoutineDefaultType::Concrete(ty) => RoutineDefaultType::Concrete(self.ty(ty)?),
            RoutineDefaultType::Polymorphic(name) => {
                RoutineDefaultType::Polymorphic(self.text(name)?)
            }
        })
    }
    fn boxed<T>(&mut self, value: Produced<T>) -> std::result::Result<Box<T>, SQLError> {
        let allocation = self.control.reserve(size_of::<T>())?;
        self.memory = self.control.combine(self.memory.take(), allocation);
        Ok(Box::new(self.take(value)))
    }
    fn boxed_type(&mut self, value: &ColumnType) -> std::result::Result<Box<ColumnType>, SQLError> {
        let value = value.clone_with_control(&self.control)?;
        self.boxed(value)
    }
    fn boxed_default(
        &mut self,
        value: &RoutineDefaultType,
    ) -> std::result::Result<Box<RoutineDefaultType>, SQLError> {
        let value = copy_default(value, &self.control)?;
        self.boxed(value)
    }
    fn finish<T>(self, value: T) -> std::result::Result<Produced<T>, SQLError> {
        Ok(self.control.finish(value, self.memory)?)
    }
}

fn signature_error(
    control: &ProductionControl<'_>,
    build: impl FnOnce(&mut Components<'_>) -> std::result::Result<RoutineSignatureMatchError, SQLError>,
) -> RoutineSignatureProductionError {
    let mut fields = Components::new(*control);
    match build(&mut fields).and_then(|error| fields.finish(error)) {
        Ok(error) => RoutineSignatureProductionError::Signature(error),
        Err(error) => RoutineSignatureProductionError::Production(error),
    }
}

fn copy_default(
    value: &RoutineDefaultType,
    control: &ProductionControl<'_>,
) -> std::result::Result<Produced<RoutineDefaultType>, SQLError> {
    let mut fields = Components::new(*control);
    let value = fields.default_type(value)?;
    fields.finish(value)
}

fn inline_type(ty: ColumnType, control: &ProductionControl<'_>) -> Result<Produced<ColumnType>> {
    debug_assert!(matches!(
        ty,
        ColumnType::Text
            | ColumnType::AnyArray
            | ColumnType::Range(_)
            | ColumnType::Multirange(_)
            | ColumnType::SmallInteger
            | ColumnType::Oid
            | ColumnType::Int2Vector
            | ColumnType::OidVector
    ));
    Ok(control.finish(ty, control.empty_reservation())?)
}

fn array_type(
    element: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>> {
    let mut fields = Components::new(*control);
    let element = element.clone_with_control(control)?;
    let array = ColumnType::Array(fields.boxed(element)?);
    Ok(fields.finish(array)?)
}

fn resource_failure(error: &SQLError) -> bool {
    matches!(error.sqlstate(), Some("53200" | "57014"))
}

fn parsed_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>> {
    match ColumnType::from_sql_name_with_control(name, control) {
        Ok(ty) => Ok(Some(ty)),
        Err(error) if resource_failure(&error) => Err(error.into()),
        Err(_) => Ok(None),
    }
}

fn polymorphic_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<RoutinePolymorphicType>> {
    let name =
        super::super::overload_resolution::canonical_routine_type_name_with_control(name, control)?;
    Ok(super::polymorphic_type_from_canonical(&name))
}
