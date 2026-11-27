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
                Ok(super::super::function_source::expand(&source, self)?.unwrap_or(source))
            }
            SourcePlan::Subquery { .. } => source.map_query(control, |query| self.query(query)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizer::{
        api::NoRegisteredAggregates, traversal::optimize_source, OptimizerConfig,
    };
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken, Value,
    };
    use uqa_sql::{
        plan::{RelationalPlan, UnifiedPlan},
        ScalarExpr,
    };

    fn group() -> SourcePlan {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(
            uqa_sql::compile("SELECT * FROM ROWS FROM (unregistered_member(1)) AS g(value)")
                .unwrap()
                .remove(0),
        ) else {
            panic!("query fixture")
        };
        let RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("query block")
        };
        let mut source = block.from.take().unwrap();
        let SourcePlan::FunctionGroup { functions, .. } = &mut source else {
            panic!("function group")
        };
        functions[0].args[0] = ScalarExpr::Literal(Value::Str("x".repeat(1 << 20)));
        source
    }

    fn argument(source: &SourcePlan) -> &str {
        let SourcePlan::FunctionGroup { functions, .. } = source else {
            panic!("function group")
        };
        let ScalarExpr::Literal(Value::Str(value)) = &functions[0].args[0] else {
            panic!("text argument")
        };
        value
    }

    #[test]
    fn unselected_function_group_keeps_its_wide_argument_without_a_member_copy() {
        let mut source = group();
        let memory = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        let config = OptimizerConfig {
            production: control,
            ..OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control)
        };
        let aggregates = NoRegisteredAggregates;
        let native = source.copy_for_rewrite(&control).unwrap();
        let reserved = native.reserved_bytes();
        let sibling = memory.reserve(memory.available() - 8192).unwrap();
        let ordinary_ptr = argument(&source).as_ptr();
        let native_ptr = argument(&native).as_ptr();
        let ordinary = allocation_counter::measure(|| {
            optimize_source(&mut source, &config, &aggregates).unwrap();
        });
        let mut output = None;
        let admitted = allocation_counter::measure(|| {
            output = Some(
                Context {
                    config: &config,
                    aggregates: &aggregates,
                }
                .source(native),
            );
        });
        let output = output.unwrap();
        assert!(
            ordinary.bytes_total < 8192 && admitted.bytes_total < 8192 && output.is_ok(),
            "caller bytes {}, admitted bytes {}, admitted error {:?}",
            ordinary.bytes_total,
            admitted.bytes_total,
            output.as_ref().err()
        );
        let output = output.unwrap();
        assert_eq!(ordinary_ptr, argument(&source).as_ptr());
        assert_eq!(native_ptr, argument(&output).as_ptr());
        assert_eq!(output.reserved_bytes(), reserved);
        drop((output, sibling));
        assert_eq!(memory.used(), 0);
    }
}
