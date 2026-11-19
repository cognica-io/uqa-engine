//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    checkpoint, visit_references, visit_references_with_work, PlanRef, SourceMember,
    SourceObserver, SourcePlan,
};
use crate::{
    ast::{ColumnType, FunctionBinding},
    catalog::work::Work,
    SQLError,
};
use std::future::Future;
use uqa_core::memory::ProductionControl;

pub(crate) enum StoredPlanOutput<'a> {
    Name(&'a str),
    Concrete(&'a ColumnType),
}

pub(crate) trait TypeObserver {
    fn binding<W: Work + Send>(
        &mut self,
        value: &FunctionBinding,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
    fn output<W: Work + Send>(
        &mut self,
        value: StoredPlanOutput<'_>,
        work: &mut W,
    ) -> impl Future<Output = Result<(), SQLError>> + Send;
}

impl<'a> SourceMember<'a> {
    fn outputs(self) -> impl Iterator<Item = Option<StoredPlanOutput<'a>>> {
        let (names, concrete): (&[String], &[Option<ColumnType>]) = match self {
            Self::Source(SourcePlan::Function { column_types, .. }) => (column_types, &[]),
            Self::Function(function) => (&function.column_types, &[]),
            Self::Source(SourcePlan::Values {
                internal_column_types,
                ..
            }) => (&[], internal_column_types),
            Self::Source(_) => (&[], &[]),
        };
        names
            .iter()
            .map(|name| Some(StoredPlanOutput::Name(name)))
            .chain(
                concrete
                    .iter()
                    .map(|value| value.as_ref().map(StoredPlanOutput::Concrete)),
            )
    }
}

/// Visit table-function declarations and retained VALUES types through the same admitted scopes as stored relation references.
pub fn visit_stored_plan_output_types(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str) -> Result<(), SQLError>,
    concrete: &mut impl FnMut(&ColumnType) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_references(plan, control, &mut |_, _| Ok(()), &mut |member| {
        for selected in member.outputs().flatten() {
            control.check()?;
            match selected {
                StoredPlanOutput::Name(ty) => visit(ty)?,
                StoredPlanOutput::Concrete(ty) => concrete(ty)?,
            }
        }
        Ok(())
    })
}

#[derive(Clone, Copy)]
enum Selection {
    Bindings,
    Outputs,
}

struct Selected<'o, O> {
    phase: Selection,
    observer: &'o mut O,
}

impl<O: TypeObserver + Send> SourceObserver for Selected<'_, O> {
    async fn source<W: Work + Send>(
        &mut self,
        member: SourceMember<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        match self.phase {
            Selection::Bindings => {
                if let Some((_, Some(binding))) = member.function() {
                    self.observer.binding(binding, work).await?;
                }
            }
            Selection::Outputs => {
                for selected in member.outputs() {
                    checkpoint(control, work).await?;
                    if let Some(selected) = selected {
                        self.observer.output(selected, work).await?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) async fn visit_plan_type_metadata<W: Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    observer: &mut (impl TypeObserver + Send),
) -> Result<(), SQLError> {
    for phase in [Selection::Bindings, Selection::Outputs] {
        visit_references_with_work(
            plan,
            control,
            work,
            &mut |_, _| Ok(()),
            &mut Selected { phase, observer },
        )
        .await?;
    }
    Ok(())
}
