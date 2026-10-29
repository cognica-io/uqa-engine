//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source planning moves each input and predicate owner in the original left-to-right sequence.

use super::{Context, MutableSourcePlan, SQLError, SourcePlan};
use uqa_sql::plan::{SourceChildSlot, SourceScalarSlot};

impl Context<'_, '_> {
    pub(super) fn source(
        &self,
        mut source: MutableSourcePlan,
    ) -> Result<MutableSourcePlan, SQLError> {
        let control = &self.config.production;
        match &*source {
            SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => Ok(source),
            SourcePlan::Join { right, lateral, .. } => {
                let was_function = matches!(
                    &**right,
                    SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
                );
                let lateral = *lateral;
                source = source
                    .map_child(SourceChildSlot::Left, control, |source| self.source(source))?;
                source = source.map_child(SourceChildSlot::Right, control, |source| {
                    self.source(source)
                })?;
                let SourcePlan::Join { right, .. } = &*source else {
                    unreachable!()
                };
                let lateral =
                    lateral || (was_function && matches!(&**right, SourcePlan::Subquery { .. }));
                source = source.set_join_lateral(lateral, control)?;
                source.map_scalar(SourceScalarSlot::JoinOn, control, |expression| {
                    self.access_predicate(self.qualification(expression)?)
                })
            }
            SourcePlan::Values { rows, .. } => {
                if rows.iter().any(|row| !row.is_empty()) {
                    let config = self.config.for_values_without_active()?;
                    let nested = Context {
                        config: &config,
                        aggregates: self.aggregates,
                        active: self.active,
                    };
                    for row in 0..rows.len() {
                        let SourcePlan::Values { rows, .. } = &*source else {
                            unreachable!()
                        };
                        for column in 0..rows[row].len() {
                            source = source.map_scalar(
                                SourceScalarSlot::Value { row, column },
                                control,
                                |expression| nested.scalar(expression),
                            )?;
                        }
                    }
                }
                Ok(source)
            }
            SourcePlan::Function { args, .. } => {
                for argument in 0..args.len() {
                    source = source.map_scalar(
                        SourceScalarSlot::Argument(argument),
                        control,
                        |expression| self.scalar(expression),
                    )?;
                }
                super::super::function_source::owned::optimize(source, self)
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for function in 0..functions.len() {
                    let SourcePlan::FunctionGroup { functions, .. } = &*source else {
                        unreachable!()
                    };
                    for argument in 0..functions[function].args.len() {
                        source = source.map_scalar(
                            SourceScalarSlot::GroupArgument { function, argument },
                            control,
                            |expression| self.scalar(expression),
                        )?;
                    }
                }
                super::super::function_source::owned::optimize_group(source, self)
            }
            SourcePlan::Subquery { .. } => source.map_query(control, |query| self.query(query)),
        }
    }
}
