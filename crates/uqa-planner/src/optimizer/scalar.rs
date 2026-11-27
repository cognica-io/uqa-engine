//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scalar simplification with SQL errors preserved at the planning boundary.

use super::{OptimizerConfig, ProjectionPlan, ScalarExpr, Value};
use uqa_sql::SQLError;

mod conditional;
pub(super) mod constants;
pub(super) mod owned;
pub(super) mod qualification;
mod vector_thresholds;
pub(crate) use qualification::optimize_qualification;
pub(crate) use qualification::optimize_qualification_owned;

use uqa_core::memory::ProductionControl;
use uqa_sql::plan::OwnedScalarExpr;

#[cfg(test)]
mod storage_tests;

pub(super) fn optimize_scalar_slot(
    expression: &mut ScalarExpr,
    config: &OptimizerConfig,
) -> Result<(), SQLError> {
    rewrite_ordinary_slot(expression, config, owned::optimize_in)
}

/// Existing caller-owned syntax can be moved without charging its original allocations again. Newly evaluated values and scratch always belong to the active operation.
#[derive(Clone, Copy)]
pub(super) struct ScalarProduction<'a, 'scope> {
    pub(super) config: &'a OptimizerConfig<'scope>,
    pub(super) storage: ProductionControl<'scope>,
}

impl<'a, 'scope> ScalarProduction<'a, 'scope> {
    pub(super) fn admitted(config: &'a OptimizerConfig<'scope>) -> Self {
        Self {
            config,
            storage: config.production,
        }
    }

    pub(super) fn caller(config: &'a OptimizerConfig<'scope>) -> Self {
        Self {
            config,
            storage: ProductionControl::uncontrolled(),
        }
    }

    pub(super) fn query_inputs(
        self,
        query: uqa_sql::plan::MutableQueryPlan,
    ) -> Result<uqa_sql::plan::MutableQueryPlan, SQLError> {
        let values = match &query.root {
            super::RelationalPlan::Values { rows, .. } => self.config.for_values(rows)?,
            super::RelationalPlan::SetOp { .. } => None,
            super::RelationalPlan::QueryBlock(_) => unreachable!("query input scalar scope"),
        };
        let config = values.as_ref().unwrap_or(self.config);
        query.map_local_scalars(&self.storage, &config.production, |expression| {
            owned::optimize_in(
                expression,
                ScalarProduction {
                    config,
                    storage: self.storage,
                },
            )
        })
    }

    pub(super) fn source_inputs(
        self,
        source: uqa_sql::plan::MutableSourcePlan,
    ) -> Result<uqa_sql::plan::MutableSourcePlan, SQLError> {
        let values = match &*source {
            super::SourcePlan::Values { rows, .. } => self.config.for_values(rows)?,
            super::SourcePlan::Function { .. } | super::SourcePlan::FunctionGroup { .. } => None,
            _ => unreachable!("range item argument scope"),
        };
        let config = values.as_ref().unwrap_or(self.config);
        source.map_local_scalars(&self.storage, &config.production, |expression| {
            owned::optimize_in(
                expression,
                ScalarProduction {
                    config,
                    storage: self.storage,
                },
            )
        })
    }

    /// Hand a newly produced constructor to the caller's syntax owner. Shared literal values retain their independent leases.
    pub(super) fn adopt(self, produced: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        if self.storage.budget().is_none() && self.config.production.budget().is_some() {
            let syntax = produced.finish(&self.config.production)?.into_parts().0;
            Ok(OwnedScalarExpr::uncontrolled(syntax))
        } else {
            Ok(produced)
        }
    }

    pub(super) fn copy_text(
        self,
        source: &str,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        let text = self.config.production.copy_text(source)?;
        if self.storage.budget().is_none() && self.config.production.budget().is_some() {
            Ok(self.storage.finish(text.into_parts().0, None)?)
        } else {
            Ok(text)
        }
    }

    pub(super) fn projection_alias(
        self,
        projection: &ProjectionPlan,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        if projection.alias.is_some()
            || matches!(
                projection.expr,
                ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
            )
        {
            return Ok(None);
        }
        uqa_sql::semantics::with_projection_label(projection, &self.config.production, |name| {
            self.copy_text(name).map(Some)
        })
    }
}

pub(super) fn rewrite_ordinary_slot(
    expression: &mut ScalarExpr,
    config: &OptimizerConfig<'_>,
    rewrite: impl FnOnce(OwnedScalarExpr, ScalarProduction<'_, '_>) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<(), SQLError> {
    let production = ScalarProduction::caller(config);
    let original = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
    let output = rewrite(OwnedScalarExpr::uncontrolled(original), production)?;
    *expression = output
        .finish(&production.storage)?
        .into_uncontrolled()
        .expect("caller-owned scalar");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::ast::BinaryOp;

    #[test]
    fn scalar_optimization_retains_array_coercion_origin() {
        for implicit in [false, true] {
            let expected = ScalarExpr::Cast {
                implicit,
                expr: Box::new(ScalarExpr::Array(vec![ScalarExpr::Column("value".into())])),
                ty: "bigint[]".into(),
            };
            let mut expression = expected.clone();
            optimize_scalar_slot(
                &mut expression,
                &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
            )
            .unwrap();
            assert_eq!(expression, expected);
        }
    }

    #[test]
    fn folds_literal_date_value_without_erasing_its_declared_type() {
        let mut expression = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(Value::Str("1993-07-01".into()))),
            ty: "date".into(),
        };

        optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();

        assert!(matches!(
            expression,
            ScalarExpr::Cast { ty, .. } if ty == "date"
        ));
    }

    #[test]
    fn folds_nested_literal_arithmetic_bottom_up() {
        let mut expression = ScalarExpr::Binary {
            op: BinaryOp::Multiply,
            lhs: Box::new(ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(2))),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(3))),
            }),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(4))),
        };

        optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap();

        assert!(matches!(
            expression,
            ScalarExpr::TypedLiteral {
                value: Value::Int(20),
                bound_type: Some(uqa_sql::ColumnType::Integer),
                ..
            }
        ));
    }

    #[test]
    fn reports_invalid_constant_input_during_planning() {
        let mut expression = ScalarExpr::Cast {
            implicit: false,
            expr: Box::new(ScalarExpr::Literal(Value::Str("not-an-integer".into()))),
            ty: "integer".into(),
        };
        let error = optimize_scalar_slot(
            &mut expression,
            &OptimizerConfig::new(uqa_execution::scalar::eval_constant_scalar_with_control),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"));
    }
}
