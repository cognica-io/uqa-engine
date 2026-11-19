//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed relation references retain lexical CTE visibility without copying stored plans.

use crate::{
    plan::{CommandPlan, CtePlan, CtePlanBody, PlanRef, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::{
    memory::{ProductionControl, ProductionMap, ProductionSmallVec},
    RelationIdentity, RelationName,
};

#[derive(Clone, Copy)]
enum Task<'a> {
    Queries(&'a [crate::plan::QueryPlan]),
    Plan(PlanRef<'a>),
    Root(&'a RelationalPlan),
    Source(&'a SourcePlan),
    Command(&'a CommandPlan),
    Ctes(&'a [CtePlan], usize, bool),
    Add(&'a str),
    Leave(&'a [CtePlan]),
}

/// Visit concrete sources and mutation targets in a stored query or SQL-standard routine body. Nonrecursive CTE bodies see outer and preceding names; recursive bodies see all siblings. Operator-join inputs and mutation targets always name real relations. The callback observes original names and inheritance selection without rebinding or evaluating anything. Scratch and cancellation use the supplied control; task order is not an execution order.
pub fn visit_stored_plan_relation_references(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_references(plan, control, visit, &mut |_| Ok(()))
}

/// Observe the function targets owned by FROM items, including ROWS FROM groups. Scalar arguments remain owned by the ordinary expression visitor; this traversal shares the stored relation visitor's admitted, iterative plan walk.
pub fn visit_stored_plan_function_bindings(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&crate::ast::FunctionBinding) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    visit_table_function_calls(plan, control, &mut |_, binding| {
        if let Some(binding) = binding {
            visit(binding)?;
        }
        Ok(())
    })
}

pub(crate) trait StoredTableFunctionObserver {
    fn function<W: crate::catalog::work::Work + Send>(
        &mut self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send;
}

struct TableFunctions<'a, V>(&'a mut V);
impl<V: StoredTableFunctionObserver> SourceObserver for TableFunctions<'_, V> {
    fn source<W: crate::catalog::work::Work + Send>(
        &mut self,
        member: SourceMember<'_>,
        _: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        let call = member
            .function()
            .map(|(name, binding)| self.0.function(name, binding, work));
        async move {
            if let Some(call) = call {
                call.await?;
            }
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
enum SourceMember<'a> {
    Source(&'a SourcePlan),
    Function(&'a crate::plan::TableFunctionPlan),
}

impl<'a> SourceMember<'a> {
    fn function(self) -> Option<(&'a str, Option<&'a crate::ast::FunctionBinding>)> {
        match self {
            Self::Source(SourcePlan::Function { name, binding, .. }) => {
                Some((name, binding.as_ref()))
            }
            Self::Function(function) => Some((&function.name, function.binding.as_ref())),
            Self::Source(_) => None,
        }
    }
}

trait SourceObserver {
    fn source<W: crate::catalog::work::Work + Send>(
        &mut self,
        member: SourceMember<'_>,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send;
}

struct Immediate<F>(F);
impl<F: FnMut(SourceMember<'_>) -> Result<(), SQLError>> SourceObserver for Immediate<F> {
    fn source<W: crate::catalog::work::Work + Send>(
        &mut self,
        member: SourceMember<'_>,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready((self.0)(member))
    }
}

pub(crate) fn visit_table_function_calls(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, Option<&crate::ast::FunctionBinding>) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    struct Inline<'a, F>(&'a mut F);
    impl<F: FnMut(&str, Option<&crate::ast::FunctionBinding>) -> Result<(), SQLError>>
        StoredTableFunctionObserver for Inline<'_, F>
    {
        fn function<W: crate::catalog::work::Work + Send>(
            &mut self,
            name: &str,
            binding: Option<&crate::ast::FunctionBinding>,
            _: &mut W,
        ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
            std::future::ready((self.0)(name, binding))
        }
    }
    crate::catalog::work::inline(visit_table_function_calls_with_work(
        plan,
        control,
        &mut (),
        &mut Inline(visit),
    ))
}

pub(crate) async fn visit_table_function_calls_with_work<W: crate::catalog::work::Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut impl StoredTableFunctionObserver,
) -> Result<(), SQLError> {
    visit_references_with_work(
        plan,
        control,
        work,
        &mut |_, _| Ok(()),
        &mut TableFunctions(visit),
    )
    .await
}

/// The stored placement determines which application registry can supply an unresolved call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredFunctionSite {
    Expression,
    Window,
    From,
    ScalarOnly,
}

/// Visit calls whose stored selection still permits runtime lookup. A selected SQL object, builtin, structural operation or retained resolution error keeps its original authority. Window calls and FROM items keep their distinct SQL placement; scalar arguments are visited by the ordinary plan expression walker.
pub fn visit_stored_runtime_calls(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(runtime_calls(plan, control, &mut (), visit))
}

/// Observe the same original call sites with resumable plan, list and scalar work.
pub async fn visit_stored_runtime_calls_from(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    runtime_calls(plan, control, input, visit).await
}

pub(crate) async fn visit_statement_runtime_calls(
    statement: &crate::ast::Statement,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    let plan = crate::plan::UnifiedPlan::lower_executable_from_input(
        statement,
        &|_: &str| false,
        control,
        input,
    )
    .await?;
    visit_stored_runtime_calls_from(plan.as_plan_ref(), control, input, visit).await
}

pub(crate) async fn visit_expression_plan_runtime_calls(
    plan: &crate::plan::ExpressionPlan,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    crate::ir::observation::visit_all(
        &plan.scalar,
        control,
        input,
        &mut RuntimeCalls {
            control: *control,
            visit,
        },
    )
    .await?;
    for query in &plan.subqueries {
        checkpoint(control, input).await?;
        visit_stored_runtime_calls_from(PlanRef::Query(query), control, input, visit).await?;
    }
    Ok(())
}

struct RuntimeCalls<'v, 'c, F> {
    control: ProductionControl<'c>,
    visit: &'v mut F,
}

impl<F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError>> RuntimeCalls<'_, '_, F> {
    fn call(
        &mut self,
        name: &str,
        binding: Option<&crate::ast::FunctionBinding>,
        site: StoredFunctionSite,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        if let Some(site) = site.with_stored_binding(binding) {
            (self.visit)(name, site)?;
        }
        Ok(())
    }
}

impl<F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError>>
    crate::ir::observation::ScalarObserver for RuntimeCalls<'_, '_, F>
{
    fn scalar<W: crate::catalog::work::Work + Send>(
        &mut self,
        expression: &crate::ScalarExpr,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready(match expression {
            crate::ScalarExpr::Func { name, binding, .. } => {
                self.call(name, binding.as_ref(), StoredFunctionSite::Expression)
            }
            crate::ScalarExpr::WindowCall { name, .. } => {
                self.call(name, None, StoredFunctionSite::Window)
            }
            _ => Ok(()),
        })
    }
}

impl<F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError>> SourceObserver
    for RuntimeCalls<'_, '_, F>
{
    fn source<W: crate::catalog::work::Work + Send>(
        &mut self,
        member: SourceMember<'_>,
        _: &ProductionControl<'_>,
        _: &mut W,
    ) -> impl std::future::Future<Output = Result<(), SQLError>> + Send {
        std::future::ready(match member.function() {
            Some((name, binding)) => self.call(name, binding, StoredFunctionSite::From),
            None => Ok(()),
        })
    }
}

async fn runtime_calls<W: crate::catalog::work::Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    scheduler: &mut W,
    visit: &mut impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let mut calls = RuntimeCalls {
        control: *control,
        visit,
    };
    crate::plan::visit_scalar_nodes_with_work(plan, control, scheduler, &mut calls).await?;
    visit_references_with_work(plan, control, scheduler, &mut |_, _| Ok(()), &mut calls).await
}

mod types;
pub use types::visit_stored_plan_output_types;
pub(crate) use types::{visit_plan_type_metadata, StoredPlanOutput, TypeObserver};

fn visit_references(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
    source_visit: &mut impl FnMut(SourceMember<'_>) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(visit_references_with_work(
        plan,
        control,
        &mut (),
        visit,
        &mut Immediate(source_visit),
    ))
}

async fn visit_references_with_work<W: crate::catalog::work::Work + Send>(
    plan: PlanRef<'_>,
    control: &ProductionControl<'_>,
    scheduler: &mut W,
    visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
    source_visit: &mut impl SourceObserver,
) -> Result<(), SQLError> {
    let mut work = Work {
        pending: ProductionSmallVec::new(*control),
        visible: ProductionMap::new(*control),
        control: *control,
    };
    work.pending.push(Task::Plan(plan))?;
    while let Some(task) = work.pending.pop() {
        checkpoint(control, scheduler).await?;
        match task {
            Task::Plan(PlanRef::Query(query)) => {
                work.enter(&query.ctes, Task::Root(&query.root), scheduler)
                    .await?;
            }
            Task::Plan(PlanRef::Command(command)) => {
                work.enter(command.ctes(), Task::Command(command), scheduler)
                    .await?;
            }
            Task::Ctes(ctes, index, recursive) => {
                if let Some(cte) = ctes.get(index) {
                    work.pending.push(Task::Ctes(ctes, index + 1, recursive))?;
                    if !recursive {
                        work.pending.push(Task::Add(&cte.name))?;
                    }
                    work.pending.push(Task::Plan(match &cte.body {
                        CtePlanBody::Query(query) => PlanRef::Query(query),
                        CtePlanBody::Command(command) => PlanRef::Command(command),
                    }))?;
                }
            }
            Task::Queries(queries) => {
                if let Some((last, rest)) = queries.split_last() {
                    if !rest.is_empty() {
                        work.pending.push(Task::Queries(rest))?;
                    }
                    work.pending.push(Task::Plan(PlanRef::Query(last)))?;
                }
            }
            Task::Add(name) => work.add(name)?,
            Task::Leave(ctes) => {
                for cte in ctes {
                    checkpoint(control, scheduler).await?;
                    let name = RelationName::decoded(&cte.name);
                    let count = work.visible.remove(&name).expect("entered CTE scope");
                    if count > 1 {
                        work.visible.insert_copy(name, count - 1)?;
                    }
                }
            }
            Task::Root(root) => work.root(root)?,
            Task::Source(source) => {
                source_visit
                    .source(SourceMember::Source(source), control, scheduler)
                    .await?;
                if let SourcePlan::FunctionGroup { functions, .. } = source {
                    for function in functions {
                        checkpoint(control, scheduler).await?;
                        source_visit
                            .source(SourceMember::Function(function), control, scheduler)
                            .await?;
                    }
                }
                work.source(source, visit, scheduler).await?;
            }
            Task::Command(command) => {
                if let Some(target) = command.mutation_target() {
                    let descendants = match command {
                        CommandPlan::Insert(plan) => plan.include_descendants,
                        CommandPlan::Update(plan) => plan.include_descendants,
                        CommandPlan::Delete(plan) => plan.include_descendants,
                        CommandPlan::Merge(plan) => plan.include_descendants,
                        _ => unreachable!("mutation target owner"),
                    };
                    visit(target, descendants)?;
                } else if !matches!(command, CommandPlan::Notify { .. } | CommandPlan::Empty) {
                    return Err(SQLError::Internal(
                        "unsupported stored command relation scope".into(),
                    ));
                }
                let (first, rest) = command.query_input_parts();
                if let Some(first) = first {
                    work.pending.push(Task::Plan(PlanRef::Query(first)))?;
                }
                if !rest.is_empty() {
                    work.pending.push(Task::Queries(rest))?;
                }
                if let Some(source) = command.source_input() {
                    work.pending.push(Task::Source(source))?;
                }
            }
        }
    }
    control.check()?;
    Ok(())
}

struct Work<'a, 'c> {
    pending: ProductionSmallVec<'c, [Task<'a>; 8]>,
    visible: ProductionMap<'c, RelationName<'a>, usize>,
    control: ProductionControl<'c>,
}

impl<'a> Work<'a, '_> {
    fn add(&mut self, name: &'a str) -> Result<(), SQLError> {
        let name = RelationName::decoded(name);
        let count = self.visible.get(&name).copied().unwrap_or(0);
        self.visible.insert_copy(
            name,
            count
                .checked_add(1)
                .ok_or_else(|| SQLError::Internal("CTE scope depth overflow".into()))?,
        )?;
        Ok(())
    }

    async fn enter<W: crate::catalog::work::Work>(
        &mut self,
        ctes: &'a [CtePlan],
        body: Task<'a>,
        scheduler: &mut W,
    ) -> Result<(), SQLError> {
        if ctes.is_empty() {
            self.pending.push(body)?;
            return Ok(());
        }
        let mut recursive = false;
        for cte in ctes {
            checkpoint(&self.control, scheduler).await?;
            recursive |= cte.recursive;
        }
        if recursive {
            for cte in ctes {
                checkpoint(&self.control, scheduler).await?;
                self.add(&cte.name)?;
            }
        }
        self.pending.push(Task::Leave(ctes))?;
        self.pending.push(body)?;
        self.pending.push(Task::Ctes(ctes, 0, recursive))?;
        Ok(())
    }

    fn root(&mut self, root: &'a RelationalPlan) -> Result<(), SQLError> {
        let subqueries = match root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(source) = &block.from {
                    self.pending.push(Task::Source(source))?;
                }
                &block.subqueries
            }
            RelationalPlan::SetOp {
                left,
                right,
                subqueries,
                ..
            } => {
                self.pending.push(Task::Plan(PlanRef::Query(left)))?;
                self.pending.push(Task::Plan(PlanRef::Query(right)))?;
                subqueries
            }
            RelationalPlan::Values { subqueries, .. } => subqueries,
        };
        if !subqueries.is_empty() {
            self.pending.push(Task::Queries(subqueries))?;
        }
        Ok(())
    }

    async fn source<W: crate::catalog::work::Work>(
        &mut self,
        source: &'a SourcePlan,
        visit: &mut impl FnMut(&str, bool) -> Result<(), SQLError>,
        scheduler: &mut W,
    ) -> Result<(), SQLError> {
        match source {
            SourcePlan::Table {
                name,
                include_descendants,
                ..
            } => {
                let reference = RelationIdentity::borrowed_reference(name).ok_or_else(|| {
                    SQLError::Internal("malformed stored relation reference".into())
                })?;
                if reference.schema.is_some() || self.visible.get(&reference.name).is_none() {
                    visit(name, *include_descendants)?;
                }
            }
            SourcePlan::Join { left, right, .. } => {
                self.pending.push(Task::Source(right))?;
                self.pending.push(Task::Source(left))?;
            }
            SourcePlan::Subquery { body, .. } => {
                self.pending.push(Task::Plan(PlanRef::Query(body)))?;
            }
            SourcePlan::Function { relations, .. } => {
                if let Some(relations) = relations {
                    visit(&relations.left, false)?;
                    self.control.check()?;
                    visit(&relations.right, false)?;
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    checkpoint(&self.control, scheduler).await?;
                    if let Some(relations) = &function.relations {
                        visit(&relations.left, false)?;
                        self.control.check()?;
                        visit(&relations.right, false)?;
                    }
                }
            }
            SourcePlan::Values { .. } | SourcePlan::RetainedRows(_) => {}
        }
        Ok(())
    }
}

async fn checkpoint<W: crate::catalog::work::Work>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<(), SQLError> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    Ok(())
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
