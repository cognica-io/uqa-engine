//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic view commands retain private syntax and metadata through every layer callback.

mod insert;
mod layers;
mod merge;
mod merge_validation;
pub use insert::rewrite_insert_owned;
pub use merge::rewrite_merge_owned;
pub use merge_validation::{
    merge_view_target_path_with_control, validate_public_merge_targets_with_control,
};
mod update_delete;
pub use update_delete::rewrite_rule_inputs_owned;

use super::{AutomaticViewLayer, ExpressionScope, SQLError, ViewCheckOption, ViewRewriteContext};
use crate::plan::{CommandPlan, CommandScalarSlot, MutableCommandPlan};
use uqa_core::memory::{Produced, ProductionControl};

fn table(input: &CommandPlan) -> &str {
    match input {
        CommandPlan::Insert(plan) => &plan.table,
        CommandPlan::Update(plan) => &plan.table,
        CommandPlan::Delete(plan) => &plan.table,
        CommandPlan::Merge(plan) => &plan.target,
        _ => unreachable!("view mutation"),
    }
}
fn qualifier(input: &CommandPlan) -> &str {
    match input {
        CommandPlan::Insert(plan) => &plan.target_qualifier,
        CommandPlan::Update(plan) => &plan.target_qualifier,
        CommandPlan::Delete(plan) => &plan.target_qualifier,
        CommandPlan::Merge(plan) => &plan.target_qualifier,
        _ => unreachable!("view mutation"),
    }
}
fn aliases(input: &CommandPlan) -> &crate::ast::ReturningAliases {
    match input {
        CommandPlan::Insert(plan) => &plan.returning_aliases,
        CommandPlan::Update(plan) => &plan.returning_aliases,
        CommandPlan::Delete(plan) => &plan.returning_aliases,
        CommandPlan::Merge(plan) => &plan.returning_aliases,
        _ => unreachable!("view mutation"),
    }
}
fn returning_count(input: &CommandPlan) -> usize {
    match input {
        CommandPlan::Insert(plan) => plan.returning.len(),
        CommandPlan::Update(plan) => plan.returning.len(),
        CommandPlan::Delete(plan) => plan.returning.len(),
        CommandPlan::Merge(plan) => plan.returning.len(),
        _ => unreachable!("view mutation"),
    }
}

fn checks(
    services: ViewRewriteContext<'_>,
    mut input: MutableCommandPlan,
    layer: &AutomaticViewLayer,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let count = match &*input {
        CommandPlan::Insert(plan) => plan.view_checks.len(),
        CommandPlan::Update(plan) => plan.view_checks.len(),
        CommandPlan::Merge(plan) => plan.view_checks.len(),
        _ => unreachable!("view check command"),
    };
    for index in 0..count {
        input = input.map_scalar_input(
            CommandScalarSlot::ViewCheck(index),
            control,
            |input, arena| {
                super::returning::owned::target_expression(
                    services,
                    input,
                    layer,
                    ExpressionScope {
                        target_qualifier: qualifier,
                        source: None,
                        returning_aliases: None,
                        include_excluded: false,
                    },
                    arena,
                    control,
                )
            },
        )?;
    }
    Ok(input)
}

fn check_option(
    services: ViewRewriteContext<'_>,
    input: MutableCommandPlan,
    layer: &AutomaticViewLayer,
    qualifier: &str,
    cascaded: &mut bool,
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    let current = *cascaded || layer.check_option != ViewCheckOption::None;
    *cascaded |= layer.check_option == ViewCheckOption::Cascaded;
    if current {
        if let Some(predicate) = &layer.predicate {
            return input.prepend_view_check(
                control.copy_text(&layer.canonical_name)?,
                control,
                |arena| {
                    arena.append_expression(
                        super::layer_rewrite::embed_owned(
                            services, predicate, layer, qualifier, control,
                        )?,
                        control,
                    )
                },
            );
        }
    }
    Ok(input)
}

fn returning(
    services: ViewRewriteContext<'_>,
    input: MutableCommandPlan,
    layer: &AutomaticViewLayer,
    source: Option<&crate::RowSchema>,
    source_first: bool,
    control: &ProductionControl<'_>,
) -> Result<(MutableCommandPlan, Produced<Vec<usize>>), SQLError> {
    let mut boundaries = None;
    let input = input.map_returning_inputs(control, |command, values, arena| {
        let scope = ExpressionScope {
            target_qualifier: qualifier(command),
            returning_aliases: Some(aliases(command)),
            source,
            include_excluded: false,
        };
        let (values, positions) = super::returning::owned::returning(
            services,
            values,
            layer,
            scope,
            source_first,
            arena,
            control,
        )?;
        boundaries = Some(positions);
        Ok(values.into_iter())
    })?;
    Ok((input, boundaries.expect("RETURNING producer")))
}

fn finish_returning(
    services: ViewRewriteContext<'_>,
    input: MutableCommandPlan,
    source: Option<&crate::RowSchema>,
    boundaries: &[usize],
    control: &ProductionControl<'_>,
) -> Result<MutableCommandPlan, SQLError> {
    input.map_returning_inputs(control, |command, values, _| {
        Ok(super::returning::owned::finalize(
            services,
            table(command),
            values,
            source,
            boundaries,
            control,
        )?
        .into_iter())
    })
}
