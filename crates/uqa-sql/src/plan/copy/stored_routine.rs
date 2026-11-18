//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently editable stored statements and parameters retain the exact allocations of the current declaration.

use super::{capture_stored_with_footprint, syntax::SyntaxCopy, CopyPurpose, Copying, Result};
use crate::ast::{CreateFunction, FunctionBody, FunctionParam, Statement};
use crate::routines::{dependencies::RoutineDeclaration, SQLRoutine};
use uqa_core::memory::{
    BudgetedVec, MemoryError, MemoryReservation, Produced, ProductionControl, ProductionString,
};

#[derive(Clone, Copy)]
struct Field {
    direct: usize,
    children: usize,
}

impl Field {
    fn slots<T>(
        count: usize,
        fields: &mut BudgetedVec<Self>,
        copying: &mut Copying<'_>,
    ) -> Result<Vec<T>> {
        fields.reserve(count)?;
        let mut values = BudgetedVec::new(fields.budget());
        values.reserve(count)?;
        let (values, memory) = values.into_parts();
        copying.absorb(Some(memory));
        Ok(values)
    }

    fn append<T: SyntaxCopy>(
        source: &T,
        values: &mut Vec<T>,
        fields: &mut BudgetedVec<Self>,
        direct: &mut MemoryReservation,
        children: &mut usize,
        control: &ProductionControl<'_>,
    ) -> Result<()> {
        let (value, lease, field) = Self::capture(source, control)?;
        let next = children
            .checked_add(field.children)
            .ok_or(MemoryError::SizeOverflow)?;
        fields.push(field)?;
        values.push(value);
        direct.absorb(lease);
        *children = next;
        Ok(())
    }

    fn capture<T: SyntaxCopy>(
        value: &T,
        control: &ProductionControl<'_>,
    ) -> Result<(T, MemoryReservation, Self)> {
        let (value, children) = capture_stored_with_footprint(value, control)?;
        let field = Self {
            direct: value.reserved_bytes(),
            children,
        };
        let (value, lease) = value.into_parts();
        Ok((value, lease.expect("native routine declaration"), field))
    }
}

/// The private declaration keeps its original stored field projection and one reservation measurement per independently replaceable item. Deferred source text, parameters and body statements are populated from the immutable routine before binding or publication.
pub struct StoredRoutineDefinition {
    source: SQLRoutine,
    definition: CreateFunction,
    parameters: BudgetedVec<Field>,
    statements: BudgetedVec<Field>,
    children: usize,
    direct: MemoryReservation,
}

impl StoredRoutineDefinition {
    pub fn new(routine: SQLRoutine, control: &ProductionControl<'_>) -> Result<Self> {
        let source = &routine.def;
        let budget = control.budget().ok_or_else(|| {
            crate::SQLError::Internal(
                "a stored routine definition requires a memory allowance".into(),
            )
        })?;
        let mut parameters = BudgetedVec::new(budget);
        let mut statements = BudgetedVec::new(budget);
        let mut copying = Copying::capture(*control, CopyPurpose::Stored);
        let definition = source.copy_definition_with(
            &mut copying,
            |copying| Field::slots(source.params.len(), &mut parameters, copying),
            |copying| match &source.body {
                FunctionBody::Source(text) => {
                    let mut value = ProductionString::new(*control);
                    value.reserve(text.len())?;
                    value.push_str_chunk(text)?;
                    Ok(FunctionBody::Source(copying.take(value.finish()?)))
                }
                FunctionBody::Statements(source) => {
                    Field::slots(source.len(), &mut statements, copying)
                        .map(FunctionBody::Statements)
                }
            },
        )?;
        let children = copying.retained_children;
        let (definition, lease) = copying.finish(definition)?.into_parts();
        let direct = lease.expect("native declaration header");
        Ok(Self {
            source: routine,
            definition,
            parameters,
            statements,
            children,
            direct,
        })
    }

    /// Copy one source-text chunk, parameter or body statement from the immutable placeholder into the private declaration.
    pub fn copy_next_field(&mut self, control: &ProductionControl<'_>) -> Result<bool> {
        control.check()?;
        if let (FunctionBody::Source(original), FunctionBody::Source(copied)) =
            (&self.source.def.body, &mut self.definition.body)
        {
            if copied.len() < original.len() {
                // The header admitted this buffer at its complete size before installing it.
                debug_assert!(copied.capacity() >= original.len());
                copied.push_str(ProductionString::first_chunk(&original[copied.len()..]));
                control.check()?;
                return Ok(self.complete());
            }
        }
        if let Some(parameter) = self.source.def.params.get(self.parameters.len()) {
            Field::append(
                parameter,
                &mut self.definition.params,
                &mut self.parameters,
                &mut self.direct,
                &mut self.children,
                control,
            )?;
        } else if let (FunctionBody::Statements(source), FunctionBody::Statements(values)) =
            (&self.source.def.body, &mut self.definition.body)
        {
            if let Some(statement) = source.get(self.statements.len()) {
                Field::append(
                    statement,
                    values,
                    &mut self.statements,
                    &mut self.direct,
                    &mut self.children,
                    control,
                )?;
            }
        }
        Ok(self.complete())
    }

    fn complete(&self) -> bool {
        self.parameters.len() == self.source.def.params.len()
            && match (&self.source.def.body, &self.definition.body) {
                (FunctionBody::Statements(statements), FunctionBody::Statements(_)) => {
                    self.statements.len() == statements.len()
                }
                (FunctionBody::Source(original), FunctionBody::Source(copied)) => {
                    copied.len() == original.len()
                }
                _ => unreachable!("retained routine body form"),
            }
    }

    pub(crate) fn into_definition(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<CreateFunction>, usize)> {
        if !self.complete() {
            return Err(crate::SQLError::Internal(
                "stored routine capture is incomplete".into(),
            ));
        }
        Ok((
            control.finish(self.definition, Some(self.direct))?,
            self.children,
        ))
    }
}

/// Analyze a private copy through the ordinary binder. Only a changed field replaces its original owner, so an error or an unchanged edit releases all scratch.
fn edit<T: SyntaxCopy>(
    value: &mut T,
    field: &mut Field,
    direct: &mut MemoryReservation,
    children: &mut usize,
    control: &ProductionControl<'_>,
    bind: impl FnOnce(&mut T) -> Result<bool>,
) -> Result<bool> {
    let (mut scratch, _lease, _) = Field::capture(value, control)?;
    if !bind(&mut scratch)? {
        return Ok(false);
    }
    let (replacement, lease, next) = Field::capture(&scratch, control)?;
    let updated = (*children - field.children)
        .checked_add(next.children)
        .ok_or(MemoryError::SizeOverflow)?;
    *value = replacement;
    drop(direct.split(field.direct));
    direct.absorb(lease);
    *children = updated;
    *field = next;
    Ok(true)
}

impl RoutineDeclaration for StoredRoutineDefinition {
    fn declaration(&self) -> &CreateFunction {
        &self.definition
    }

    fn edit_statement(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        bind: impl FnOnce(&mut Statement) -> Result<bool>,
    ) -> Result<Option<bool>> {
        let FunctionBody::Statements(values) = &mut self.definition.body else {
            return Ok(None);
        };
        let Some(value) = values.get_mut(index) else {
            return Ok(None);
        };
        edit(
            value,
            &mut self.statements[index],
            &mut self.direct,
            &mut self.children,
            control,
            bind,
        )
        .map(Some)
    }

    fn edit_parameter(
        &mut self,
        index: usize,
        control: &ProductionControl<'_>,
        bind: impl FnOnce(&mut FunctionParam) -> Result<bool>,
    ) -> Result<Option<bool>> {
        let Some(value) = self.definition.params.get_mut(index) else {
            return Ok(None);
        };
        edit(
            value,
            &mut self.parameters[index],
            &mut self.direct,
            &mut self.children,
            control,
            bind,
        )
        .map(Some)
    }
}

#[cfg(test)]
mod tests;
