//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consuming scoped visitation keeps constructor ownership and admitted lexical names through command and source rewrites.

use super::{
    CommandQuerySlot, MutableCommandPlan, MutableExecutablePlan, MutableQueryPlan,
    MutableSourcePlan, QueryChildSlot, SourceChildSlot,
};
use crate::{
    plan::{CommandPlan, CtePlan, RelationalPlan, SourcePlan},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub trait OwnedScopedPlanVisitor {
    fn command(
        &mut self,
        command: MutableCommandPlan,
        _outer: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableCommandPlan, bool), SQLError> {
        Ok((command, true))
    }
    fn source(
        &mut self,
        source: MutableSourcePlan,
        _visible: &dyn Fn(&str) -> bool,
    ) -> Result<(MutableSourcePlan, bool), SQLError> {
        Ok((source, true))
    }
}

impl MutableExecutablePlan {
    pub fn visit_scoped(
        self,
        control: &ProductionControl<'_>,
        visitor: &mut dyn OwnedScopedPlanVisitor,
    ) -> Result<Self, SQLError> {
        plan(self, &|_| false, visitor, control)
    }
}

fn names(
    ctes: &[CtePlan],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut names = ProductionVec::new(*control);
    for cte in ctes {
        names.push_produced(control.copy_text(&cte.name)?)?;
    }
    Ok(names.finish()?)
}
fn plan(
    input: MutableExecutablePlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn OwnedScopedPlanVisitor,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    control.check()?;
    Ok(match input {
        MutableExecutablePlan::Query(input) => {
            MutableExecutablePlan::Query(query(input, outer, visitor, control)?)
        }
        MutableExecutablePlan::Command(input) => {
            MutableExecutablePlan::Command(command(input, outer, visitor, control)?)
        }
    })
}
fn query(
    mut input: MutableQueryPlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn OwnedScopedPlanVisitor,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    let names = names(&input.ctes, control)?;
    let recursive = input.ctes.iter().any(|cte| cte.recursive);
    for index in 0..names.len() {
        let visible = |name: &str| {
            names[..if recursive { names.len() } else { index }]
                .iter()
                .any(|candidate| candidate == name)
                || outer(name)
        };
        input = input.map_cte(index, control, |body| {
            plan(body, &visible, visitor, control)
        })?;
    }
    let visible = |name: &str| names.iter().any(|candidate| candidate == name) || outer(name);
    match &input.root {
        RelationalPlan::QueryBlock(_) => {
            input = input.map_source(control, |input| source(input, &visible, visitor, control))?;
        }
        RelationalPlan::SetOp { .. } => {
            input = input.map_query_child(QueryChildSlot::Left, control, |input| {
                query(input, &visible, visitor, control)
            })?;
            input = input.map_query_child(QueryChildSlot::Right, control, |input| {
                query(input, &visible, visitor, control)
            })?;
        }
        RelationalPlan::Values { .. } => {}
    }
    let count = match &input.root {
        RelationalPlan::QueryBlock(block) => block.subqueries.len(),
        RelationalPlan::SetOp { subqueries, .. } | RelationalPlan::Values { subqueries, .. } => {
            subqueries.len()
        }
    };
    for index in 0..count {
        input = input.map_query_child(QueryChildSlot::Subquery(index), control, |input| {
            query(input, &visible, visitor, control)
        })?;
    }
    Ok(input)
}
fn source(
    input: MutableSourcePlan,
    visible: &dyn Fn(&str) -> bool,
    visitor: &mut dyn OwnedScopedPlanVisitor,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    let (mut input, descend) = visitor.source(input, visible)?;
    if !descend {
        return Ok(input);
    }
    match &*input {
        SourcePlan::Subquery { .. } => {
            input.map_query(control, |input| query(input, visible, visitor, control))
        }
        SourcePlan::Table {
            name,
            executable_query: Some(_),
            ..
        } => {
            let cte = match uqa_core::RelationIdentity::borrowed_reference(name)
                .filter(|reference| reference.schema.is_none())
            {
                Some(reference) => visible(&control.format(format_args!("{}", reference.name))?),
                None => false,
            };
            input.map_query(control, |input| {
                query(
                    input,
                    if cte { visible } else { &|_| false },
                    visitor,
                    control,
                )
            })
        }
        SourcePlan::Join { .. } => {
            input = input.map_child(SourceChildSlot::Left, control, |input| {
                source(input, visible, visitor, control)
            })?;
            input.map_child(SourceChildSlot::Right, control, |input| {
                source(input, visible, visitor, control)
            })
        }
        _ => Ok(input),
    }
}
fn command(
    input: MutableCommandPlan,
    outer: &dyn Fn(&str) -> bool,
    visitor: &mut dyn OwnedScopedPlanVisitor,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    control.check()?;
    let (mut input, descend) = visitor.command(input, outer)?;
    if !descend {
        return Ok(input);
    }
    match &*input {
        CommandPlan::CreateView { .. }
        | CommandPlan::CreateTableAs { .. }
        | CommandPlan::CreateMaterializedView { .. }
        | CommandPlan::DeclareCursor { .. } => {
            return input.map_query_body(control, |input| query(input, outer, visitor, control))
        }
        CommandPlan::Explain { .. } | CommandPlan::Prepare { .. } => {
            return input.map_executable_body(control, |input| plan(input, outer, visitor, control))
        }
        CommandPlan::Call { args, .. } => {
            let count = args.len();
            for argument in 0..count {
                let CommandPlan::Call { args, .. } = &*input else {
                    unreachable!("retained CALL")
                };
                for index in 0..args[argument].subqueries.len() {
                    input = input.map_query_child(
                        CommandQuerySlot::CallArgumentSubquery {
                            argument,
                            query: index,
                        },
                        control,
                        |input| query(input, outer, visitor, control),
                    )?;
                }
            }
            return Ok(input);
        }
        CommandPlan::Execute { params, .. } => {
            let count = params.len();
            for parameter in 0..count {
                let CommandPlan::Execute { params, .. } = &*input else {
                    unreachable!("retained EXECUTE")
                };
                for index in 0..params[parameter].subqueries.len() {
                    input = input.map_query_child(
                        CommandQuerySlot::ExecuteParameterSubquery {
                            parameter,
                            query: index,
                        },
                        control,
                        |input| query(input, outer, visitor, control),
                    )?;
                }
            }
            return Ok(input);
        }
        CommandPlan::Insert(_)
        | CommandPlan::Update(_)
        | CommandPlan::Delete(_)
        | CommandPlan::Merge(_) => {}
        _ => return Ok(input),
    }
    let names = names(input.ctes(), control)?;
    let recursive = input.ctes().iter().any(|cte| cte.recursive);
    for index in 0..names.len() {
        let visible = |name: &str| {
            names[..if recursive { names.len() } else { index }]
                .iter()
                .any(|candidate| candidate == name)
                || outer(name)
        };
        input = input.map_cte(index, control, |body| {
            plan(body, &visible, visitor, control)
        })?;
    }
    let visible = |name: &str| names.iter().any(|candidate| candidate == name) || outer(name);
    input = input.map_source(control, |input| source(input, &visible, visitor, control))?;
    if matches!(&*input,CommandPlan::Insert(insert) if insert.source.is_some()) {
        input = input.map_query_child(CommandQuerySlot::InsertSource, control, |input| {
            query(input, &visible, visitor, control)
        })?;
    }
    let count = input.scalar_subqueries().len();
    for index in 0..count {
        input = input.map_query_child(CommandQuerySlot::Subquery(index), control, |input| {
            query(input, &visible, visitor, control)
        })?;
    }
    Ok(input)
}
