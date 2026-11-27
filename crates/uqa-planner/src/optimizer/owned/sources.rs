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
            SourcePlan::Values { .. }
            | SourcePlan::Function { .. }
            | SourcePlan::FunctionGroup { .. } => {
                source = super::super::scalar::ScalarProduction::admitted(self.config)
                    .source_inputs(source)?;
                match &*source {
                    SourcePlan::Function { .. } => {
                        super::super::function_source::owned::optimize(source, self)
                    }
                    SourcePlan::FunctionGroup { .. } => {
                        super::super::function_source::owned::optimize_group(source, self)
                    }
                    _ => Ok(source),
                }
            }
            SourcePlan::Subquery { .. } => source.map_query(control, |query| self.query(query)),
        }
    }
}
