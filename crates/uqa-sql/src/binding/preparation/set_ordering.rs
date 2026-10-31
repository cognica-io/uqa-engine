//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Set-operation sorting analyzes SQL92 output references before rejecting computed targets.

use super::{error, Preparation, QueryOutput, QueryPlan, SQLError, ScalarExpr};
use crate::plan::{OrderPlan, RelationalPlan};
use uqa_core::Value;

impl Preparation<'_> {
    pub(super) fn set_ordering(
        &mut self,
        order_by: &[OrderPlan],
        output: &QueryOutput,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let schema = output.schema(&self.scope.control())?;
        let mut computed = false;
        for order in order_by {
            match &order.expr {
                // The parser's integer token fits positive int4; a folded minus preserves that magnitude. Larger integer-valued numeric tokens are not SQL92 ordinals.
                ScalarExpr::Literal(Value::Int(position))
                    if (-i64::from(i32::MAX)..=i64::from(i32::MAX)).contains(position) => {}
                ScalarExpr::Literal(_) => {
                    return Err(error("42601", "non-integer constant in ORDER BY".into()));
                }
                ScalarExpr::Column(name) => {
                    if schema.column_is_ambiguous(name) {
                        return Err(error("42702", format!("ORDER BY \"{name}\" is ambiguous")));
                    }
                }
                ScalarExpr::Position(_) => {}
                _ => computed = true,
            }
            let value = self
                .alias_expression(
                    &order.expr,
                    output,
                    &schema,
                    &schema,
                    subqueries,
                    "ORDER BY",
                )
                .map_err(|error| output.set_column_error(error))?;
            if let Some(ty) = value.ty {
                crate::type_resolution::ComparisonSupport::Ordering
                    .require_in_catalog(&ty, self.routines)?;
            }
            self.parameters
                .collations
                .require_key(&order.expr, value.derivation)?;
        }
        if computed {
            return Err(SQLError::Diagnostic {
                sqlstate: "0A000".into(),
                message: "invalid UNION/INTERSECT/EXCEPT ORDER BY clause".into(),
                detail: Some("Only result column names can be used, not expressions or functions.".into()),
                hint: Some("Add the expression/function to every SELECT, or move the UNION into a FROM clause.".into()),
            });
        }
        Ok(())
    }
}

impl QueryOutput {
    pub(super) fn retain_set_operand_names(&mut self, query: &QueryPlan) -> Result<(), SQLError> {
        let nested = query.ctes.is_empty()
            && matches!(&query.root, RelationalPlan::SetOp { order_by, limit, offset, .. }
                if order_by.is_empty() && limit.is_none() && offset.is_none());
        if !nested {
            let control = self.control.control();
            let mut columns = super::ownership::AnalysisVec::new(&control);
            columns.reserve(self.columns.len(), &control)?;
            for column in &self.columns {
                columns.push(control.copy_text(column)?, &control)?;
            }
            self.set_sources.clear();
            self.set_sources.push(columns, &control)?;
        }
        Ok(())
    }

    fn set_column_error(&self, error: SQLError) -> SQLError {
        let SQLError::UnknownColumn(column) = error.primary() else {
            return error;
        };
        let mut matches = self
            .set_sources
            .iter()
            .enumerate()
            .filter(|(_, names)| names.iter().any(|name| name.as_str() == column));
        let Some((position, _)) = matches.next() else {
            return error;
        };
        let multiple = matches.next().is_some();
        SQLError::Diagnostic {
            sqlstate: "42703".into(),
            message: error.to_string(),
            detail: Some(if multiple {
                format!("There are columns named \"{column}\", but they are in tables that cannot be referenced from this part of the query.")
            } else {
                format!("There is a column named \"{column}\" in table \"*SELECT* {}\", but it cannot be referenced from this part of the query.", position + 1)
            }),
            hint: multiple.then(|| "Try using a table-qualified name.".into()),
        }
        .at_position(error.position())
    }
}
