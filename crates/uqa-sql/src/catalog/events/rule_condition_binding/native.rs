//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured rule row identities are observed without reallocating or refreshing them.

use super::RuleConditionBinding;
use crate::{ast::RuleEvent, plan::ExpressionPlan, SQLError, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl RuleConditionBinding {
    pub(crate) fn validate_captured(
        &self,
        event: RuleEvent,
        plan: &ExpressionPlan,
        control: &ProductionControl<'_>,
        column: &mut impl FnMut(&str) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let old = matches!(event, RuleEvent::Update | RuleEvent::Delete);
        let new = matches!(event, RuleEvent::Insert | RuleEvent::Update);
        if self.old_relation.is_some() != old || self.new_relation.is_some() != new {
            return Err(invalid("row sides differ from the rule event", control));
        }
        if self.old_relation.is_some() && self.old_relation == self.new_relation {
            return Err(invalid("OLD and NEW share an identity", control));
        }
        if [self.old_relation, self.new_relation]
            .into_iter()
            .flatten()
            .any(|relation| relation.raw() == 0)
        {
            return Err(invalid("zero row identity", control));
        }
        let mut names = ProductionMap::new(*control);
        for name in &self.columns {
            if names.insert_copy(name.as_str(), ())?.is_some() {
                return Err(invalid("duplicate captured column", control));
            }
        }
        let mut inspect = |expression: &ScalarExpr| {
            expression.try_visit(&mut |node| {
                control.check()?;
                let reference = match node {
                    ScalarExpr::InternalColumn(reference) => Some(*reference),
                    ScalarExpr::Func {
                        binding: Some(binding),
                        ..
                    } if crate::semantics::function_has_implicit_row_input(binding) => {
                        return Err(invalid("execution-only implicit row input", control));
                    }
                    _ => None,
                };
                if let Some(reference) = reference {
                    let name = self.column_name(reference).ok_or_else(|| {
                        invalid("unowned or out-of-range captured column", control)
                    })?;
                    column(name)?;
                }
                Ok::<_, SQLError>(true)
            })
        };
        inspect(&plan.scalar)?;
        for query in &plan.subqueries {
            crate::plan::PlanRef::Query(query).try_visit_scalar_expressions(&mut inspect)?;
        }
        control.check()?;
        Ok(())
    }
}

fn invalid(detail: &str, control: &ProductionControl<'_>) -> SQLError {
    match control.format(format_args!("invalid captured rule condition: {detail}")) {
        Ok(message) => SQLError::Internal(message.into_parts().0),
        Err(error) => error.into(),
    }
}
