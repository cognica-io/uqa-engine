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
        let access = uqa_sql::ast::ArrayAccess::from_dispatch(dispatch, args.len(), |index| {
            matches!(args[index], ScalarExpr::Literal(Value::Null))
        })
        .ok_or_else(|| SQLError::Internal("not an array access".into()))??;
        access.validate_dimensions()?;
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
        if access.slice_bounds().is_some() {
            for (dimension, bounds) in indices.as_chunks::<2>().0.iter().enumerate() {
                let lower = self.array_bound(
                    &bounds[0],
                    access.is_omitted(dimension * 2 + 1),
                    scope,
                    subqueries,
                )?;
                let upper = self.array_bound(
                    &bounds[1],
                    access.is_omitted(dimension * 2 + 2),
                    scope,
                    subqueries,
                )?;
                write!(rendered, "[{lower}:{upper}]").expect("writing to a String cannot fail");
            }
        } else {
            for index in indices {
                write!(
                    rendered,
                    "[{}]",
                    self.array_bound(index, false, scope, subqueries)?
                )
                .expect("writing to a String cannot fail");
            }
        }
        Ok(rendered)
    }

    fn array_bound(
        &self,
        bound: &ScalarExpr,
        omitted: bool,
        scope: &Scope,
        subqueries: &[QueryPlan],
    ) -> Result<String, SQLError> {
        if omitted {
            Ok(String::new())
        } else {
            let mut bound = bound;
            while let ScalarExpr::Cast {
                expr,
                implicit: true,
                ..
            } = bound
            {
                if matches!(expr.as_ref(), ScalarExpr::Literal(Value::Null)) {
                    break;
                }
                bound = expr;
            }
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
                    index_key_definition(None, &catalog, &resolution, &index.columns[0], pretty)
                        .unwrap(),
                    expected,
                    "{input}, pretty={pretty}",
                );
            }
        }
    }
}
