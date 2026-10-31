//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Plan the selected CHECK once, then evaluate borrowed VALUE inputs without copying stored rows.

use super::DomainValidationInputs;
use crate::{
    query::{catalog_expression::PreparedCatalogExpression, CteScope},
    RowSchema,
};
use uqa_core::Value;
use uqa_sql::{ast::DomainCheck, catalog::domain::StoredDomain, expr::RowLookup, SQLError};

pub(super) struct PreparedCheck<'a>(PreparedCatalogExpression<'a>);

impl<'a> PreparedCheck<'a> {
    pub(super) fn new<S: Clone + 'static>(
        inputs: &DomainValidationInputs<'a, S>,
        domain: &StoredDomain,
        check: &DomainCheck,
    ) -> Result<Self, SQLError> {
        let resources = inputs.expressions.production_resources()?;
        let control = resources.control();
        let scope = CteScope::with_catalog_with_control(
            inputs.catalog.catalog.current_catalog_snapshot(),
            inputs
                .catalog
                .session
                .relation_name_resolution_with_control(&control)?,
            &control,
        )?;
        let schema = RowSchema::with_column_metadata_with_control(
            None,
            [("value", Some(&domain.definition.base), None)],
            &control,
        )?;
        Ok(Self(PreparedCatalogExpression::lower(
            inputs.expressions,
            scope,
            &check.expression,
            &schema,
            &[],
        )?))
    }

    pub(super) fn violates(&self, value: &Value) -> Result<bool, SQLError> {
        Ok(self.0.evaluate_value(&DomainValue(value))? == Value::Bool(false))
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
