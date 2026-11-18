//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently editable stored statements and parameters retain the exact allocations of the current declaration.

use super::{capture_stored_with_footprint, syntax::SyntaxCopy, CopyPurpose, Copying, Result};
use crate::ast::{CreateFunction, FunctionBody, FunctionParam, Statement};
use crate::routines::{dependencies::RoutineDeclaration, SQLRoutine};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, Produced, ProductionControl};

#[derive(Clone, Copy)]
struct Field {
    direct: usize,
    children: usize,
}

impl Field {
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

/// The private declaration keeps its original stored field projection and one reservation measurement per independently replaceable item. Its statement vector is populated in source order.
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
        let mut direct = budget.empty_reservation();
        let mut children = 0usize;
        let mut copying = Copying::capture(*control, CopyPurpose::Stored);
        let definition = source.copy_definition_with(
            &mut copying,
            |copying| {
                parameters.reserve(source.params.len())?;
                copying.map(source.params.iter(), |_, value| {
                    let (value, lease, field) = Field::capture(value, control)?;
                    children = children
                        .checked_add(field.children)
                        .ok_or(MemoryError::SizeOverflow)?;
                    direct.absorb(lease);
                    parameters.push(field)?;
                    Ok(value)
                })
            },
            |copying| match &source.body {
                body @ FunctionBody::Source(_) => body.copy_syntax(copying),
                FunctionBody::Statements(source) => {
                    statements.reserve(source.len())?;
                    let mut output = BudgetedVec::new(budget);
                    output.reserve(source.len())?;
                    let (output, lease) = output.into_parts();
                    copying.absorb(Some(lease));
                    Ok(FunctionBody::Statements(output))
                }
            },
        )?;
        children = children
            .checked_add(copying.retained_children)
            .ok_or(MemoryError::SizeOverflow)?;
        let (definition, lease) = copying.finish(definition)?.into_parts();
        direct.absorb(lease.expect("native declaration header"));
        Ok(Self {
            source: routine,
            definition,
            parameters,
            statements,
            children,
            direct,
        })
    }

    /// Transfer one complete statement from the immutable placeholder into the admitted writable body.
    pub fn copy_next_statement(&mut self, control: &ProductionControl<'_>) -> Result<bool> {
        control.check()?;
        let (FunctionBody::Statements(source), FunctionBody::Statements(destination)) =
            (&self.source.def.body, &mut self.definition.body)
        else {
            return Ok(true);
        };
        let Some(value) = source.get(destination.len()) else {
            return Ok(true);
        };
        let (value, lease, field) = Field::capture(value, control)?;
        self.children = self
            .children
            .checked_add(field.children)
            .ok_or(MemoryError::SizeOverflow)?;
        self.direct.absorb(lease);
        self.statements.push(field)?;
        destination.push(value);
        Ok(destination.len() == source.len())
    }

    pub(crate) fn into_definition(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<(Produced<CreateFunction>, usize)> {
        if let FunctionBody::Statements(statements) = &self.source.def.body {
            if self.statements.len() != statements.len() {
                return Err(crate::SQLError::Internal(
                    "stored routine capture is incomplete".into(),
                ));
            }
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
