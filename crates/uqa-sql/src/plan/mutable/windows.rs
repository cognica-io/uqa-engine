//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical window refresh follows the existing query-local traversal while every replacement retains its constructor owner.

use super::{
    CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, OwnedScalarExpr, QueryChildSlot, QueryScalarSlot, SourceChildSlot,
    SourceScalarSlot,
};
use crate::{
    plan::{CommandPlan, RelationalPlan, SourcePlan},
    SQLError, ScalarExpr, ScalarWindowSpec,
};
use uqa_core::memory::ProductionControl;

mod roots;

impl MutableExecutablePlan {
    pub fn normalize_window_definitions(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(match self {
            Self::Query(query) => Self::Query(query.normalize_window_definitions(control)?),
            Self::Command(command) => Self::Command(command.normalize_window_definitions(control)?),
        })
    }
}
impl MutableQueryPlan {
    pub fn normalize_window_definitions(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        for index in 0..self.ctes.len() {
            self = self.map_cte(index, control, |plan| {
                plan.normalize_window_definitions(control)
            })?;
        }
        let subqueries = match &self.root {
            RelationalPlan::QueryBlock(block) => block.subqueries.len(),
            RelationalPlan::SetOp { subqueries, .. }
            | RelationalPlan::Values { subqueries, .. } => subqueries.len(),
        };
        match &self.root {
            RelationalPlan::QueryBlock(_) => {
                self = self.map_source(control, |source| normalize_source(source, control))?;
            }
            RelationalPlan::SetOp { .. } => {
                self = self.map_query_child(QueryChildSlot::Left, control, |query| {
                    query.normalize_window_definitions(control)
                })?;
                self = self.map_query_child(QueryChildSlot::Right, control, |query| {
                    query.normalize_window_definitions(control)
                })?;
            }
            RelationalPlan::Values { .. } => {}
        }
        for index in 0..subqueries {
            self = self.map_query_child(QueryChildSlot::Subquery(index), control, |query| {
                query.normalize_window_definitions(control)
            })?;
        }
        if let RelationalPlan::QueryBlock(block) = &self.root {
            let resolved =
                crate::plan::windows::resolved_window_definitions_with_control(block, control)?;
            self = roots::normalize(self, &resolved, control)?;
        }
        self.prune_subqueries(control)
    }
}
impl MutableCommandPlan {
    pub fn normalize_window_definitions(
        mut self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        for index in 0..self.ctes().len() {
            self = self.map_cte(index, control, |plan| {
                plan.normalize_window_definitions(control)
            })?;
        }
        self = self.map_source(control, |source| normalize_source(source, control))?;
        let subqueries = match &*self {
            CommandPlan::Insert(plan) => {
                let count = plan.subqueries.len();
                if plan.source.is_some() {
                    self =
                        self.map_query_child(CommandQuerySlot::InsertSource, control, |query| {
                            query.normalize_window_definitions(control)
                        })?;
                }
                count
            }
            CommandPlan::Update(plan) => plan.subqueries.len(),
            CommandPlan::Delete(plan) => plan.subqueries.len(),
            CommandPlan::Merge(plan) => plan.subqueries.len(),
            _ => 0,
        };
        for index in 0..subqueries {
            self = self.map_query_child(CommandQuerySlot::Subquery(index), control, |query| {
                query.normalize_window_definitions(control)
            })?;
        }
        match &*self {
            CommandPlan::CreateView { .. }
            | CommandPlan::CreateMaterializedView { .. }
            | CommandPlan::CreateTableAs { .. }
            | CommandPlan::DeclareCursor { .. } => {
                self.map_query_body(control, |query| query.normalize_window_definitions(control))
            }
            CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
                self.map_executable_body(control, |body| body.normalize_window_definitions(control))
            }
            CommandPlan::Execute { params, .. } | CommandPlan::Call { args: params, .. } => {
                let count = params.len();
                for parameter in 0..count {
                    let (call, queries) = match &*self {
                        CommandPlan::Call { args, .. } => (true, args[parameter].subqueries.len()),
                        CommandPlan::Execute { params, .. } => {
                            (false, params[parameter].subqueries.len())
                        }
                        _ => unreachable!(),
                    };
                    for query in 0..queries {
                        let slot = if call {
                            CommandQuerySlot::CallArgumentSubquery {
                                argument: parameter,
                                query,
                            }
                        } else {
                            CommandQuerySlot::ExecuteParameterSubquery { parameter, query }
                        };
                        self = self.map_query_child(slot, control, |query| {
                            query.normalize_window_definitions(control)
                        })?;
                    }
                }
                Ok(self)
            }
            _ => Ok(self),
        }
    }
}
fn normalize_source(
    mut source: MutableSourcePlan,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*source {
        SourcePlan::Subquery { .. } => {
            source.map_query(control, |query| query.normalize_window_definitions(control))
        }
        SourcePlan::Join { .. } => {
            source = source.map_child(SourceChildSlot::Left, control, |source| {
                normalize_source(source, control)
            })?;
            source.map_child(SourceChildSlot::Right, control, |source| {
                normalize_source(source, control)
            })
        }
        _ => Ok(source),
    }
}
fn scalar(
    mut expression: OwnedScalarExpr,
    resolved: &[ScalarWindowSpec],
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    control.check()?;
    if let ScalarExpr::WindowCall { spec, .. } = &*expression {
        if let Some(slot) = spec.definition {
            let canonical = resolved.get(slot).ok_or_else(|| {
                super::super::windows::invalid("a call refers to a missing window definition")
            })?;
            expression = expression.replace_window_spec(canonical, control)?;
        }
    }
    if let ScalarExpr::WindowCall { args, filter, .. } = &*expression {
        let arguments = args.len();
        let filter = filter.is_some();
        let children = expression.child_count();
        for index in 0..arguments {
            expression =
                expression.map_child(index, control, |child| scalar(child, resolved, control))?;
        }
        if filter {
            expression = expression.map_child(children - 1, control, |child| {
                scalar(child, resolved, control)
            })?;
        }
        for index in arguments..children - usize::from(filter) {
            expression =
                expression.map_child(index, control, |child| scalar(child, resolved, control))?;
        }
        Ok(expression)
    } else {
        expression.map_children(control, |_, child| scalar(child, resolved, control))
    }
}
