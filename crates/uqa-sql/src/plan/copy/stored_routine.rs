//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently editable stored statements and parameters retain the exact allocations of the current declaration.

use super::{
    capture_stored, capture_stored_with_control, syntax::SyntaxCopy, CopyPurpose, Copying, Result,
};
use crate::ast::{CreateFunction, FunctionBody, FunctionParam, Statement};
use crate::routines::{dependencies::RoutineDeclaration, SQLRoutine};
use uqa_core::memory::{BudgetedVec, MemoryError, MemoryReservation, Produced, ProductionControl};

mod edit;
mod header;
pub use edit::StoredRoutineParameterEdit;
mod parameter;
pub(super) use header::HeaderCopy;
use header::{continue_text, start_text, Header};

#[derive(Clone, Copy)]
struct Field<MergeTargets = ()> {
    direct: usize,
    children: usize,
    has_merge_targets: MergeTargets,
}

impl<MergeTargets> Field<MergeTargets> {
    fn slots<T>(
        count: usize,
        fields: &mut BudgetedVec<Self>,
        copying: &mut Copying<'_>,
    ) -> Result<Vec<T>> {
        fields.reserve(count)?;
        copying.reserve_slots(count)
    }

    fn append<T: SyntaxCopy>(
        source: &T,
        values: &mut Vec<T>,
        fields: &mut BudgetedVec<Self>,
        direct: &mut MemoryReservation,
        children: &mut usize,
        control: &ProductionControl<'_>,
        merge_targets: impl FnOnce(&Copying<'_>) -> MergeTargets,
    ) -> Result<()> {
        let (value, lease, field) = Self::capture(source, control, merge_targets)?;
        Self::install(value, lease, field, values, fields, direct, children)
    }

    fn install<T>(
        value: T,
        lease: MemoryReservation,
        field: Self,
        values: &mut Vec<T>,
        fields: &mut BudgetedVec<Self>,
        direct: &mut MemoryReservation,
        children: &mut usize,
    ) -> Result<()> {
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
        merge_targets: impl FnOnce(&Copying<'_>) -> MergeTargets,
    ) -> Result<(T, MemoryReservation, Self)> {
        let (value, (children, has_merge_targets)) = capture_stored(value, control, |copying| {
            (copying.retained_children, merge_targets(copying))
        })?;
        Ok(Self::from_produced(value, children, has_merge_targets))
    }

    fn from_produced<T>(
        value: Produced<T>,
        children: usize,
        has_merge_targets: MergeTargets,
    ) -> (T, MemoryReservation, Self) {
        let field = Self {
            direct: value.reserved_bytes(),
            children,
            has_merge_targets,
        };
        let (value, lease) = value.into_parts();
        (value, lease.expect("native routine declaration"), field)
    }
}

/// The private declaration keeps its original stored field projection and one reservation measurement per independently replaceable item. Deferred source text, parameters and body statements are populated from the immutable routine before binding or publication.
pub struct StoredRoutineDefinition {
    source: SQLRoutine,
    definition: CreateFunction,
    header: Header,
    pending_parameter: Option<parameter::Capture>,
    parameters: BudgetedVec<Field>,
    statements: BudgetedVec<Field<bool>>,
    children: usize,
    direct: MemoryReservation,
}

/// The compiler borrows the current declaration unless a dropped MERGE target requires its own executable projection.
pub struct StoredStatementCompilation {
    position: usize,
    normalized: Option<Produced<Box<Statement>>>,
}

impl StoredStatementCompilation {
    pub fn lower<'a, 'control: 'a, 'input: 'a>(
        &'a self,
        definition: &'a StoredRoutineDefinition,
        aggregates: &'a (dyn crate::plan::AggregateClassifier + Sync),
        control: &'a ProductionControl<'control>,
        input: &'a mut uqa_core::image_io::AsyncImageInput<'input>,
    ) -> impl std::future::Future<Output = Result<crate::plan::RetainedExecutablePlan>>
           + Send
           + 'a
           + use<'a, 'control, 'input> {
        let FunctionBody::Statements(values) = &definition.definition.body else {
            unreachable!("compilation holds its SQL-standard declaration")
        };
        let statement = self
            .normalized
            .as_deref()
            .map_or(&values[self.position], Box::as_ref);
        async move {
            crate::plan::UnifiedPlan::lower_executable_from_input(
                statement, aggregates, control, input,
            )
            .await?
            .upgrade_stored_dispatches(control, input)
            .await
        }
    }
}

impl StoredRoutineDefinition {
    pub fn compilation_statement(
        &mut self,
        position: usize,
        catalog: &dyn crate::routines::merge_columns::StoredMergeColumnCatalog,
        control: &ProductionControl<'_>,
    ) -> Result<StoredStatementCompilation> {
        let FunctionBody::Statements(statements) = &mut self.definition.body else {
            unreachable!("SQL-standard compilation")
        };
        let statement = &mut statements[position];
        crate::routines::compilation::validate_sql_standard_statement(statement)?;
        let mut normalized = None;
        if self.statements[position].has_merge_targets
            && crate::routines::merge_columns::contains_removed_merge_target(catalog, statement)?
        {
            let (mut scratch, lease) =
                capture_stored_with_control(statement, control)?.into_parts();
            crate::routines::merge_columns::normalize_stored_merge_target_columns(
                catalog,
                &mut scratch,
            )?;
            let mut copying = Copying::capture(*control, CopyPurpose::Stored);
            let selected = copying.boxed(|copying| scratch.copy_syntax(copying))?;
            normalized = Some(copying.finish(selected)?);
            drop(scratch);
            drop(lease);
        }
        Ok(StoredStatementCompilation {
            position,
            normalized,
        })
    }
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
            HeaderCopy::Retained,
            |copying| Field::slots(source.params.len(), &mut parameters, copying),
            |copying| match &source.body {
                FunctionBody::Source(text) => Ok(FunctionBody::Source(
                    copying.take(start_text(text, control)?),
                )),
                FunctionBody::Statements(source) => {
                    Field::slots(source.len(), &mut statements, copying)
                        .map(FunctionBody::Statements)
                }
            },
        )?;
        let children = copying.retained_children;
        let (definition, lease) = copying.finish(definition)?.into_parts();
        let direct = lease.expect("native declaration header");
        let mut capture = Self {
            source: routine,
            definition,
            header: Header::default(),
            pending_parameter: None,
            parameters,
            statements,
            children,
            direct,
        };
        capture.copy_header(control)?;
        Ok(capture)
    }

    fn copy_header(&mut self, control: &ProductionControl<'_>) -> Result<()> {
        let mut copying = Copying::capture(*control, CopyPurpose::Stored);
        let result = self
            .header
            .copy_window(&self.source.def, &mut self.definition, &mut copying);
        // Even a failed window can already own completed items and part of a setting.
        self.direct
            .absorb(copying.memory.expect("controlled declaration"));
        self.children = self
            .children
            .checked_add(copying.retained_children)
            .ok_or(MemoryError::SizeOverflow)?;
        result
    }

    /// Advance the captured header, source text, parameters and body of the immutable placeholder before binding its private declaration.
    pub fn copy_next_field(&mut self, control: &ProductionControl<'_>) -> Result<bool> {
        let pending = self.pending_parameter.take();
        control.check()?;
        if !self.header.complete() {
            self.copy_header(control)?;
            return Ok(self.complete());
        }
        if let (FunctionBody::Source(original), FunctionBody::Source(copied)) =
            (&self.source.def.body, &mut self.definition.body)
        {
            if copied.len() < original.len() {
                continue_text(original, copied);
                control.check()?;
                return Ok(self.complete());
            }
        }
        if let Some(parameter) = self.source.def.params.get(self.parameters.len()) {
            let (value, lease, field) = if parameter.default.is_some() {
                let mut capture = match pending {
                    Some(capture) => capture,
                    None => parameter::Capture::new(
                        self.source.clone(),
                        self.parameters.len(),
                        control,
                    )?,
                };
                let Some((produced, children)) = capture.advance(control)? else {
                    self.pending_parameter = Some(capture);
                    return Ok(false);
                };
                Field::from_produced(produced, children, ())
            } else {
                Field::capture(parameter, control, |_| ())?
            };
            Field::install(
                value,
                lease,
                field,
                &mut self.definition.params,
                &mut self.parameters,
                &mut self.direct,
                &mut self.children,
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
                    |copying| copying.has_merge_targets,
                )?;
            }
        }
        Ok(self.complete())
    }

    fn complete(&self) -> bool {
        self.header.complete()
            && self.parameters.len() == self.source.def.params.len()
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
fn edit<T: SyntaxCopy, MergeTargets>(
    value: &mut T,
    field: &mut Field<MergeTargets>,
    direct: &mut MemoryReservation,
    children: &mut usize,
    control: &ProductionControl<'_>,
    bind: impl FnOnce(&mut T) -> Result<bool>,
    merge_targets: impl FnOnce(&Copying<'_>) -> MergeTargets,
) -> Result<bool> {
    let (mut scratch, _lease) = capture_stored_with_control(value, control)?.into_parts();
    if !bind(&mut scratch)? {
        return Ok(false);
    }
    edit::Target {
        value,
        field,
        direct,
        children,
    }
    .replace(Field::capture(&scratch, control, merge_targets)?)
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
            |copying| copying.has_merge_targets,
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
            |_| (),
        )
        .map(Some)
    }
}

#[cfg(test)]
mod tests;
