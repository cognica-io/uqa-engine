//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field selections, as `get_rule_expr` prints a `FieldSelect`.

use uqa_core::Value;
use uqa_sql::ast::FunctionDispatch;
use uqa_sql::ir::ScalarExpr;
use uqa_sql::plan::QueryPlan;

use super::{quote_ident, Deparser, SQLError, Scope};

impl Deparser<'_> {
    /// The argument in parentheses unless it is a subscript or another field selection, then the field. A field of a whole-row reference is the relation's column, as parse analysis resolves it.
    pub(super) fn field_selection(
        &self,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let [base, ScalarExpr::Literal(Value::Str(field))] = args else {
            return Err(SQLError::Internal(
                "field selection takes an expression and a field name".into(),
            ));
        };
        if let ScalarExpr::Column(qualifier) | ScalarExpr::QualifiedStar(qualifier) = base {
            if !scope.resolves(None, qualifier) && scope.has_relation(qualifier) {
                return Ok(self.column_reference(Some(qualifier), field, scope));
            }
        }
        let bare = matches!(
            base,
            ScalarExpr::Func {
                binding: Some(binding),
                ..
            } if matches!(
                binding.dispatch,
                Some(
                    FunctionDispatch::ArraySubscripts
                        | FunctionDispatch::ArraySlices
                        | FunctionDispatch::FieldSelect
                )
            )
        );
        let base = self.expression(base, scope, subqueries)?;
        Ok(if bare {
            format!("{base}.{}", quote_ident(field))
        } else {
            format!("({base}).{}", quote_ident(field))
        })
    }
}
