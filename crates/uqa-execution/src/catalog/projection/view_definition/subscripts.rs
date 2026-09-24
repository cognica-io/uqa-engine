//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstruct typed array access without confusing its display name with a SQL routine.

use std::fmt::Write as _;
use uqa_core::Value;
use uqa_sql::{ast::FunctionDispatch, ir::ScalarExpr, plan::QueryPlan, SQLError};

use super::{Deparser, Scope};

impl Deparser<'_> {
    pub(super) fn array_subscripts(
        &self,
        dispatch: FunctionDispatch,
        args: &[ScalarExpr],
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        let Some((array, indices)) = args.split_first() else {
            return Err(SQLError::Internal("array access has no operand".into()));
        };
        if indices.is_empty()
            || (dispatch == FunctionDispatch::ArraySlices && !indices.len().is_multiple_of(2))
        {
            return Err(SQLError::Internal("invalid array access bounds".into()));
        }
        let array_sql = self.expression(array, scope, subqueries)?;
        let mut rendered = if matches!(
            array,
            ScalarExpr::Column(_) | ScalarExpr::QualifiedColumn { .. } | ScalarExpr::Position(_)
        ) {
            array_sql
        } else {
            // Nested accesses need their own parentheses to preserve their dimensional grouping.
            format!("({array_sql})")
        };
        if dispatch == FunctionDispatch::ArraySlices {
            for bounds in indices.chunks_exact(2) {
                let lower = self.slice_bound(&bounds[0], scope, subqueries)?;
                let upper = self.slice_bound(&bounds[1], scope, subqueries)?;
                write!(rendered, "[{lower}:{upper}]").expect("writing to a String cannot fail");
            }
        } else {
            for index in indices {
                write!(rendered, "[{}]", self.expression(index, scope, subqueries)?)
                    .expect("writing to a String cannot fail");
            }
        }
        Ok(rendered)
    }

    fn slice_bound(
        &self,
        bound: &ScalarExpr,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if matches!(bound, ScalarExpr::Literal(Value::Null)) {
            Ok(String::new())
        } else {
            self.expression(bound, scope, subqueries)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::catalog::{
        projection::helpers::index_definitions::index_key_definition, test_support::empty_catalog,
        RelationLookupMode, RelationNameResolution,
    };

    #[test]
    fn stored_array_keys_follow_postgresql_subscript_and_slice_syntax() {
        let catalog = empty_catalog();
        let resolution = RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "uqa".into(),
            lookup_mode: RelationLookupMode::Dynamic,
        };
        // PostgreSQL 18 pg_get_indexdef(index, 1, pretty) supplies both output forms.
        for (input, expected, pretty_expected) in [
            ("value[2]", "(value[2])", "(value[2])"),
            ("value[1][2]", "(value[1][2])", "(value[1][2])"),
            ("value[1:2]", "(value[1:2])", "(value[1:2])"),
            ("value[:][2]", "(value[:][1:2])", "(value[:][1:2])"),
            ("value[2:]", "(value[2:])", "(value[2:])"),
            ("(value[1:2])[2]", "((value[1:2])[2])", "((value[1:2])[2])"),
            (
                "(ARRAY[1,2])[1]",
                "((ARRAY[1, 2])[1])",
                "((ARRAY[1, 2])[1])",
            ),
            (
                "(value || value)[2]",
                "(((value || value))[2])",
                "((value || value)[2])",
            ),
            ("value[1] % 2", "((value[1] % 2))", "(value[1] % 2)"),
            (
                "subscript(value, 2)",
                "subscript(value, 2)",
                "subscript(value, 2)",
            ),
            (
                "slice(value, 1, 2)",
                "slice(value, 1, 2)",
                "slice(value, 1, 2)",
            ),
        ] {
            let uqa_sql::Statement::CreateIndex(index) =
                uqa_sql::compile(&format!("CREATE INDEX probe ON items (({input}))"))
                    .unwrap()
                    .remove(0)
            else {
                panic!("expected index");
            };
            for (pretty, expected) in [(false, expected), (true, pretty_expected)] {
                assert_eq!(
                    index_key_definition(&catalog, &resolution, &index.columns[0], pretty).unwrap(),
                    expected,
                    "{input}, pretty={pretty}",
                );
            }
        }
    }
}
