//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan the selected CHECK once, then evaluate borrowed VALUE inputs without copying stored rows.

use super::DomainValidationInputs;
use crate::{
    query::{set_projection::SetFunctionRuntime, CteScope},
    scalar::plan::{eval_physical, PhysicalEvalContext},
    RowSchema,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    ast::DomainCheck, catalog::domain::StoredDomain, expr::RowLookup, plan::ExpressionPlan,
    SQLError,
};

pub(super) struct PreparedCheck<'a> {
    expression: ExpressionPlan,
    schema: RowSchema,
    hook: Arc<dyn SetFunctionRuntime + 'a>,
}

impl<'a> PreparedCheck<'a> {
    pub(super) fn new<S: Clone + 'static>(
        inputs: &DomainValidationInputs<'a, S>,
        domain: &StoredDomain,
        check: &DomainCheck,
    ) -> Result<Self, SQLError> {
        let mut expression = ExpressionPlan::lower(check.expression.clone());
        let mut scope = CteScope::with_catalog(
            inputs.catalog.catalog.current_catalog_snapshot(),
            inputs.catalog.session.relation_name_resolution(),
            None,
        );
        scope.scalar_subqueries.clone_from(&expression.subqueries);
        let hook = inputs.expressions.bind_scope(scope);
        let schema = RowSchema::with_types(
            vec!["value".into()],
            vec![Some(domain.definition.base.clone())],
        );
        crate::scalar_type_with_resolver(&expression.scalar, &schema, &[], hook.as_ref())?;
        expression.scalar = crate::bind_type_introspection_with_resolver(
            expression.scalar,
            &schema,
            &[],
            hook.as_ref(),
        );
        (inputs.plan_check)(&mut expression.scalar)?;
        Ok(Self {
            expression,
            schema,
            hook,
        })
    }

    pub(super) fn violates(&self, value: &Value) -> Result<bool, SQLError> {
        let row = DomainValue(value);
        let context = PhysicalEvalContext::from_row_lookup(&row, &[])
            .with_row_schema(&self.schema)
            .with_function_hook(self.hook.as_ref())
            .with_subquery_runner(self.hook.as_ref());
        Ok(eval_physical(&self.expression, &context)? == Value::Bool(false))
    }
}

struct DomainValue<'a>(&'a Value);

impl RowLookup for DomainValue<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        (name == "value").then_some(self.0)
    }

    fn qualified_column(&self, _: &str, _: &str) -> Option<&Value> {
        None
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        (index == 0).then_some(self.0)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        visitor("value", self.0);
    }
}
