//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source fields and mutation targets retain one ordered binding implementation for inline and scheduled traversal.

use super::{Produced, ProductionControl, SQLError};
use crate::{
    ast::OperatorJoinRelations,
    catalog::work::{self, Work},
    plan::{CommandPlan, MutableCommandPlan, MutableSourcePlan, SourcePlan},
};
use std::future::{ready, Future};

type Result<T> = std::result::Result<T, SQLError>;

pub(crate) type StoredRelationResolver<'a> =
    dyn Fn(&str, bool, &ProductionControl<'_>) -> Result<(Produced<String>, bool)> + Sync + 'a;

pub(crate) trait Input {
    type Work: Work + Send;
    fn work(&mut self) -> &mut Self::Work;
    fn visible(
        &mut self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<bool>> + Send;
    fn resolve(
        &mut self,
        name: &str,
        descendants: bool,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>>> + Send;
}

pub(super) struct Immediate<'a, 'r> {
    pub(super) visible: &'a dyn Fn(&str) -> bool,
    pub(super) resolve: &'a mut super::Resolver<'r>,
    pub(super) work: (),
}
impl Input for Immediate<'_, '_> {
    type Work = ();
    fn work(&mut self) -> &mut Self::Work {
        &mut self.work
    }
    fn visible(
        &mut self,
        name: &str,
        _control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<bool>> + Send {
        ready(Ok((self.visible)(name)))
    }
    fn resolve(
        &mut self,
        name: &str,
        descendants: bool,
        control: &ProductionControl<'_>,
    ) -> impl Future<Output = Result<Produced<String>>> + Send {
        ready((self.resolve)(name, descendants, control))
    }
}

pub(crate) async fn command<I: Input>(
    value: &mut MutableCommandPlan,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<()> {
    let Some(target) = value.mutation_target() else {
        return Ok(());
    };
    let (resolve_descendants, descendants) = match &**value {
        CommandPlan::Update(plan) => (plan.include_descendants, plan.include_descendants),
        CommandPlan::Delete(plan) => (plan.include_descendants, plan.include_descendants),
        CommandPlan::Insert(plan) => (false, plan.include_descendants),
        CommandPlan::Merge(plan) => (false, plan.include_descendants),
        _ => unreachable!("mutation target"),
    };
    let target = input.resolve(target, resolve_descendants, control).await?;
    value.retarget_mutation_in_place(target, descendants, control)?;
    value.mark_stored_cte_target_bound_in_place(control)
}

pub(crate) async fn source<I: Input>(
    value: &mut MutableSourcePlan,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<()> {
    match &**value {
        SourcePlan::Table {
            name,
            qualifier,
            include_descendants,
            ..
        } => {
            let qualifier = if qualifier.is_empty() {
                Some(local_name(name, control, input.work()).await?)
            } else {
                None
            };
            let is_cte = match work::reference(name, control, input.work()).await? {
                Some(reference) if reference.schema.is_none() => {
                    let local = work::decoded_name(reference.name, control, input.work()).await?;
                    input.visible(&local, control).await?
                }
                _ => false,
            };
            let name = if is_cte {
                None
            } else {
                Some(input.resolve(name, *include_descendants, control).await?)
            };
            value.replace_table_reference_in_place(name, qualifier, control)?;
        }
        SourcePlan::Function {
            name,
            output_name,
            relations,
            ..
        } => {
            let output = if output_name.is_empty() {
                Some(local_name(name, control, input.work()).await?)
            } else {
                None
            };
            let relations = match relations {
                Some(relations) => Some(bound_relations(relations, control, input).await?),
                None => None,
            };
            value.replace_function_references_in_place(0, output, relations, control)?;
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            let count = functions.len();
            for index in 0..count {
                control.check()?;
                input.work().checkpoint().await?;
                let SourcePlan::FunctionGroup { functions, .. } = &**value else {
                    unreachable!()
                };
                let function = &functions[index];
                let output = if function.output_name.is_empty() {
                    Some(local_name(&function.name, control, input.work()).await?)
                } else {
                    None
                };
                let relations = match &function.relations {
                    Some(relations) => Some(bound_relations(relations, control, input).await?),
                    None => None,
                };
                value.replace_function_references_in_place(index, output, relations, control)?;
            }
        }
        SourcePlan::Join { .. }
        | SourcePlan::Subquery { .. }
        | SourcePlan::RetainedRows(_)
        | SourcePlan::Values { .. } => {}
    }
    Ok(())
}

async fn bound_relations<I: Input>(
    relations: &OperatorJoinRelations,
    control: &ProductionControl<'_>,
    input: &mut I,
) -> Result<Produced<OperatorJoinRelations>> {
    let left = input.resolve(&relations.left, false, control).await?;
    let right = input.resolve(&relations.right, false, control).await?;
    let (left, left_memory) = left.into_parts();
    let (right, right_memory) = right.into_parts();
    Ok(control.finish(
        OperatorJoinRelations { left, right },
        control.combine(left_memory, right_memory),
    )?)
}

async fn local_name<W: Work + Send>(
    name: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<String>> {
    match work::reference(name, control, work).await? {
        Some(reference) => work::decoded_name(reference.name, control, work).await,
        None => work.text(name, control).await,
    }
}
