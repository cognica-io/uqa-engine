//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View subquery alpha-renaming moves query owners and replaces only names that bind in the selected namespace.

use super::source_plan_declares_qualifier;
use crate::{
    plan::{
        CommandPlan, CommandQuerySlot, MutableExecutablePlan, MutableQueryPlan, MutableSourcePlan,
        OwnedScalarExpr, QueryChildSlot, RelationalPlan, SourceChildSlot, SourcePlan,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(super) fn query(
    mut input: MutableQueryPlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    for index in 0..input.ctes.len() {
        input = input.map_cte(index, control, |input| {
            cte(input, qualifier, replacement, inherited, control)
        })?;
    }
    let active = match &input.root {
        RelationalPlan::QueryBlock(_) => {
            input = input.map_source(control, |input| {
                source_queries(input, qualifier, replacement, inherited, control)
            })?;
            let RelationalPlan::QueryBlock(block) = &input.root else {
                unreachable!()
            };
            let declared = block
                .from
                .as_ref()
                .is_some_and(|source| source_plan_declares_qualifier(source, qualifier));
            if declared {
                input = input.map_source(control, |input| {
                    source_name(input, qualifier, replacement, control)
                })?;
            }
            let active = inherited || declared;
            if active {
                input = input.map_source(control, |input| {
                    super::scalars::source(input, control, &mut |input| {
                        scalar(input, qualifier, replacement, control)
                    })
                })?;
            }
            active
        }
        RelationalPlan::SetOp { .. } => {
            for slot in [QueryChildSlot::Left, QueryChildSlot::Right] {
                input = input.map_query_child(slot, control, |input| {
                    query(input, qualifier, replacement, inherited, control)
                })?;
            }
            inherited
        }
        RelationalPlan::Values { .. } => inherited,
    };
    if active {
        input = super::scalars::query(
            input,
            super::scalars::Order::Rename,
            control,
            &mut |input| scalar(input, qualifier, replacement, control),
        )?;
    }
    let count = super::scalars::query_count(&input);
    for index in 0..count {
        input = input.map_query_child(QueryChildSlot::Subquery(index), control, |input| {
            query(input, qualifier, replacement, active, control)
        })?;
    }
    Ok(input)
}

fn cte(
    input: MutableExecutablePlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    control.check()?;
    Ok(match input {
        MutableExecutablePlan::Query(input) => {
            MutableExecutablePlan::Query(query(input, qualifier, replacement, inherited, control)?)
        }
        MutableExecutablePlan::Command(mut input) => {
            for index in 0..input.ctes().len() {
                input = input.map_cte(index, control, |input| {
                    cte(input, qualifier, replacement, inherited, control)
                })?;
            }
            if matches!(&*input, CommandPlan::Insert(insert) if insert.source.is_some()) {
                input =
                    input.map_query_child(CommandQuerySlot::InsertSource, control, |input| {
                        query(input, qualifier, replacement, inherited, control)
                    })?;
            }
            for index in 0..input.scalar_subqueries().len() {
                input =
                    input.map_query_child(CommandQuerySlot::Subquery(index), control, |input| {
                        query(input, qualifier, replacement, inherited, control)
                    })?;
            }
            input = input.map_source(control, |input| {
                source_queries(input, qualifier, replacement, inherited, control)
            })?;
            MutableExecutablePlan::Command(input)
        }
    })
}

fn source_queries(
    mut input: MutableSourcePlan,
    qualifier: &str,
    replacement: &str,
    inherited: bool,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    match &*input {
        SourcePlan::Join { .. } => {
            for slot in [SourceChildSlot::Left, SourceChildSlot::Right] {
                input = input.map_child(slot, control, |input| {
                    source_queries(input, qualifier, replacement, inherited, control)
                })?;
            }
            Ok(input)
        }
        SourcePlan::Subquery { .. } => input.map_query(control, |input| {
            query(input, qualifier, replacement, inherited, control)
        }),
        _ => Ok(input),
    }
}

fn source_name(
    mut input: MutableSourcePlan,
    qualifier: &str,
    replacement: &str,
    control: &ProductionControl<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    control.check()?;
    if matches!(&*input, SourcePlan::Join { .. }) {
        for slot in [SourceChildSlot::Left, SourceChildSlot::Right] {
            input = input.map_child(slot, control, |input| {
                source_name(input, qualifier, replacement, control)
            })?;
        }
    }
    let alias = match &*input {
        SourcePlan::RetainedRows(_) => false,
        SourcePlan::Table {
            qualifier: current,
            alias: None,
            ..
        } if current == qualifier => {
            return input.replace_table_reference(
                None,
                Some(control.copy_text(replacement)?),
                control,
            );
        }
        SourcePlan::Table { alias, .. }
        | SourcePlan::Join { alias, .. }
        | SourcePlan::Values { alias, .. }
        | SourcePlan::Subquery { alias, .. } => alias.as_deref() == Some(qualifier),
        SourcePlan::Function {
            output_name, alias, ..
        } => alias.as_deref().unwrap_or(output_name) == qualifier,
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            }) == Some(qualifier)
        }
    };
    if alias {
        input = input.replace_alias(control.copy_text(replacement)?, control)?;
    }
    Ok(input)
}

fn scalar(
    input: OwnedScalarExpr,
    qualifier: &str,
    replacement: &str,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    input.transform_nodes(control, &mut |_, input| {
        if input.reference_qualifier() == Some(qualifier) {
            Ok(input.replace_qualifier(replacement, control)?)
        } else {
            Ok(input)
        }
    })
}
