//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select physical join keys from the statement's declared SQL operators.

use super::{
    decide_join_sides, join_conjuncts, JoinKind, RowSchema, SQLError, SQLParam, ScalarExpr,
};
use uqa_sql::type_resolution::ComparisonKeySupport;
use uqa_sql::{ast::BinaryOp, FunctionTypeResolver};

pub(super) struct HashJoinKeys {
    pub left: Vec<ScalarExpr>,
    pub right: Vec<ScalarExpr>,
    pub residual: Option<ScalarExpr>,
}

pub(super) fn join_keys(
    kind: JoinKind,
    predicate: Option<&ScalarExpr>,
    left: &RowSchema,
    right: &RowSchema,
    params: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
) -> Result<Option<HashJoinKeys>, SQLError> {
    let Some(predicate) = predicate.filter(|_| kind != JoinKind::Cross) else {
        return Ok(None);
    };
    let mut keys = HashJoinKeys {
        left: Vec::new(),
        right: Vec::new(),
        residual: None,
    };
    let mut residual = Vec::new();
    let mut merge_supported = true;
    let schema = RowSchema::join(left, right, []);
    for conjunct in join_conjuncts(predicate) {
        // ROW equality selects an operator for each field before either side is assigned to a join input. One field can use the reverse orientation or retain a volatile predicate while another supplies a hash key.
        let pairs = match conjunct {
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs,
                rhs,
            } => match (lhs.as_ref(), rhs.as_ref()) {
                (ScalarExpr::Row(left), ScalarExpr::Row(right)) if left.len() == right.len() => {
                    Some(left.iter().zip(right).collect::<Vec<_>>())
                }
                _ => Some(vec![(lhs.as_ref(), rhs.as_ref())]),
            },
            _ => None,
        };
        if let Some(pairs) = pairs {
            let mut all_hashed = !pairs.is_empty();
            for (left_key, right_key) in pairs {
                let bound = uqa_sql::bind_type_introspection_with_resolver(
                    ScalarExpr::Binary {
                        op: BinaryOp::Equal,
                        lhs: Box::new(left_key.clone()),
                        rhs: Box::new(right_key.clone()),
                    },
                    &schema,
                    params,
                    types,
                );
                if uqa_sql::semantics::volatility::expr_contains_volatile_function(
                    volatility, &bound,
                ) {
                    merge_supported = false;
                    all_hashed = false;
                    continue;
                }
                let ScalarExpr::Binary { lhs, rhs, .. } = bound else {
                    unreachable!("binding preserves a comparison operator")
                };
                let Some((left_key, right_key)) = decide_join_sides(left, right, &lhs, &rhs) else {
                    // Constants remain predicates; they cannot make a non-joinable record clause into a hash join.
                    merge_supported &= uqa_sql::type_resolution::comparison_key_is_constant(&lhs)
                        && uqa_sql::type_resolution::comparison_key_is_constant(&rhs);
                    all_hashed = false;
                    continue;
                };
                let mut support = ComparisonKeySupport::default();
                for key in [left_key, right_key] {
                    match crate::scalar_type_with_resolver(key, &schema, params, types)? {
                        Some(ty) => support.include(&ty, types)?,
                        None => {
                            // Schemaless UQA inputs require runtime comparison; absent type metadata is not a declared lack of operators.
                            support.hashing = false;
                        }
                    }
                }
                merge_supported &= support.ordering;
                all_hashed &= support.hashing;
                if support.hashing {
                    keys.left.push(left_key.clone());
                    keys.right.push(right_key.clone());
                }
            }
            if all_hashed {
                continue;
            }
        } else {
            merge_supported &= matches!(
                conjunct,
                ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
            );
        }
        residual.push(conjunct.clone());
    }
    if keys.left.is_empty() {
        if kind == JoinKind::Full && !merge_supported {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "FULL JOIN is only supported with merge-joinable or hash-joinable join conditions".into(),
            });
        }
        return Ok(None);
    }
    keys.residual = match residual.len() {
        0 => None,
        1 => residual.pop(),
        _ => Some(ScalarExpr::And(residual)),
    };
    Ok(Some(keys))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_sql::ColumnType;

    struct Types;

    impl FunctionTypeResolver for Types {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&uqa_sql::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }
    }

    impl uqa_sql::semantics::volatility::VolatilityCatalog for Types {
        fn host_function_volatility(&self, _: &str) -> Option<uqa_sql::ast::FunctionVolatility> {
            None
        }
        fn routine_volatilities(
            &self,
            _: &str,
            _: Option<&uqa_sql::ast::FunctionBinding>,
        ) -> Option<Vec<uqa_sql::ast::FunctionVolatility>> {
            None
        }
        fn view_query(&self, _: &str) -> Result<Option<uqa_sql::plan::QueryPlan>, SQLError> {
            Ok(None)
        }
    }

    fn schema(qualifier: &str, ty: Option<ColumnType>) -> RowSchema {
        RowSchema::with_qualified_types(qualifier, vec!["v".into()], vec![ty])
    }

    fn equality(row: bool) -> ScalarExpr {
        let key = |qualifier: &str| {
            let column = ScalarExpr::qualified_column(qualifier, "v");
            if row {
                ScalarExpr::Row(vec![column])
            } else {
                column
            }
        };
        ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs: Box::new(key("l")),
            rhs: Box::new(key("r")),
        }
    }

    #[test]
    fn join_selection_requires_declared_hash_support_without_requiring_ordering() {
        for (ty, hashing) in [
            (Some(ColumnType::Record), false),
            (Some(ColumnType::Xid), true),
            (None, false),
        ] {
            let left = schema("l", ty.clone());
            let right = schema("r", ty);
            let selected = join_keys(
                JoinKind::Inner,
                Some(&equality(false)),
                &left,
                &right,
                &[],
                &Types,
                &Types,
            )
            .unwrap();
            assert_eq!(selected.is_some(), hashing);
        }
    }

    #[test]
    fn row_constructor_keys_keep_scalar_null_rules_and_full_join_capabilities() {
        let left = schema("l", Some(ColumnType::Integer));
        let right = schema("r", Some(ColumnType::Integer));
        let selected = join_keys(
            JoinKind::Full,
            Some(&equality(true)),
            &left,
            &right,
            &[],
            &Types,
            &Types,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected.left, [ScalarExpr::qualified_column("l", "v")]);
        assert_eq!(selected.right, [ScalarExpr::qualified_column("r", "v")]);
        assert!(selected.residual.is_none());
        let left = schema("l", Some(ColumnType::Json));
        let right = schema("r", Some(ColumnType::Json));
        let error = join_keys(
            JoinKind::Full,
            Some(&equality(false)),
            &left,
            &right,
            &[],
            &Types,
            &Types,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("0A000"));
    }
}
