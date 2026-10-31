//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select physical join keys from the statement's declared SQL operators.

use super::{JoinKind, RowSchema, SQLError, SQLParam, ScalarExpr};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::semantics::join_predicates::visit_join_conjuncts_with_control;
use uqa_sql::{ast::BinaryOp, FunctionTypeResolver};

mod binding;
use binding::KeyBinding;

pub(super) struct HashJoinKeys {
    pub left: Produced<Vec<ScalarExpr>>,
    pub right: Produced<Vec<ScalarExpr>>,
    pub residual: Option<Produced<ScalarExpr>>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "join selection shares the original SQL and resource contexts"
)]
pub(super) fn join_keys(
    kind: JoinKind,
    predicate: Option<&ScalarExpr>,
    left: &RowSchema,
    right: &RowSchema,
    params: &[SQLParam],
    types: &dyn FunctionTypeResolver,
    volatility: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    control: &ProductionControl<'_>,
) -> Result<Option<HashJoinKeys>, SQLError> {
    control.check()?;
    let Some(predicate) = predicate.filter(|_| kind != JoinKind::Cross) else {
        return Ok(None);
    };
    let binding = KeyBinding {
        left,
        right,
        params,
        types,
        volatility,
        control: *control,
        schema: RowSchema::join_with_control(left, right, [], control)?,
    };
    let mut left_keys = ProductionVec::new(*control);
    let mut right_keys = ProductionVec::new(*control);
    let mut residual = ProductionVec::new(*control);
    let mut merge_supported = true;
    visit_join_conjuncts_with_control(predicate, control, &mut |conjunct| {
        // ROW equality selects each field's SQL operator before assigning either operand to a physical input.
        let mut pair = |lhs: &ScalarExpr, rhs: &ScalarExpr| -> Result<bool, SQLError> {
            let selected = binding.select(lhs, rhs)?;
            merge_supported &= selected.merge_supported;
            if let Some([left, right]) = selected.keys {
                left_keys.push_produced(left)?;
                right_keys.push_produced(right)?;
                Ok(true)
            } else {
                Ok(false)
            }
        };
        let all_hashed = if let ScalarExpr::Binary {
            op: BinaryOp::Equal,
            lhs,
            rhs,
        } = conjunct
        {
            if let (ScalarExpr::Row(left_items), ScalarExpr::Row(right_items)) =
                (lhs.as_ref(), rhs.as_ref())
            {
                if left_items.len() == right_items.len() {
                    let mut all = !left_items.is_empty();
                    for (lhs, rhs) in left_items.iter().zip(right_items) {
                        all &= pair(lhs, rhs)?;
                    }
                    all
                } else {
                    pair(lhs, rhs)?
                }
            } else {
                pair(lhs, rhs)?
            }
        } else {
            merge_supported &= matches!(
                conjunct,
                ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
            );
            false
        };
        if !all_hashed {
            residual.push_produced(control.finish(conjunct, control.empty_reservation())?)?;
        }
        Ok(())
    })?;
    if left_keys.is_empty() {
        if kind == JoinKind::Full && !merge_supported {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "FULL JOIN is only supported with merge-joinable or hash-joinable join conditions".into(),
            });
        }
        return Ok(None);
    }
    let residual = match &*residual {
        [] => None,
        [only] => Some(only.clone_with_control(control)?),
        conjuncts => {
            let mut owned = ProductionVec::new(*control);
            owned.reserve(conjuncts.len())?;
            for conjunct in conjuncts {
                owned.push_produced(conjunct.clone_with_control(control)?)?;
            }
            let (items, memory) = owned.finish()?.into_parts();
            Some(control.finish(ScalarExpr::And(items), memory)?)
        }
    };
    Ok(Some(HashJoinKeys {
        left: left_keys.finish()?,
        right: right_keys.finish()?,
        residual,
    }))
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
                &ProductionControl::uncontrolled(),
            )
            .unwrap();
            assert_eq!(selected.is_some(), hashing);
        }
    }

    #[test]
    fn cross_datetime_joins_preserve_comparison_predicates_and_full_join_admission() {
        for (left_type, right_type) in [
            (ColumnType::Date, ColumnType::Timestamp),
            (ColumnType::Timestamp, ColumnType::Date),
            (ColumnType::Date, ColumnType::TimestampTzPrecision(6)),
            (ColumnType::TimestampTzPrecision(6), ColumnType::Date),
        ] {
            let left = schema("l", Some(left_type));
            let right = schema("r", Some(right_type));
            for row in [false, true] {
                for kind in [
                    JoinKind::Inner,
                    JoinKind::Left,
                    JoinKind::Right,
                    JoinKind::Full,
                ] {
                    assert!(join_keys(
                        kind,
                        Some(&equality(row)),
                        &left,
                        &right,
                        &[],
                        &Types,
                        &Types,
                        &ProductionControl::uncontrolled(),
                    )
                    .unwrap()
                    .is_none());
                }
            }
        }
    }

    #[test]
    fn mixed_datetime_join_keeps_an_independent_hash_key_and_temporal_residual() {
        let schema = |qualifier, ty| {
            RowSchema::with_qualified_types(
                qualifier,
                vec!["v".into(), "id".into()],
                vec![Some(ty), Some(ColumnType::Integer)],
            )
        };
        let left = schema("l", ColumnType::Date);
        let right = schema("r", ColumnType::TimestampTz);
        let predicate = ScalarExpr::And(vec![
            equality(false),
            ScalarExpr::Binary {
                op: BinaryOp::Equal,
                lhs: Box::new(ScalarExpr::qualified_column("l", "id")),
                rhs: Box::new(ScalarExpr::qualified_column("r", "id")),
            },
        ]);
        let keys = join_keys(
            JoinKind::Full,
            Some(&predicate),
            &left,
            &right,
            &[],
            &Types,
            &Types,
            &ProductionControl::uncontrolled(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            keys.left.as_slice(),
            vec![ScalarExpr::qualified_column("l", "id")]
        );
        assert_eq!(
            keys.right.as_slice(),
            vec![ScalarExpr::qualified_column("r", "id")]
        );
        assert_eq!(keys.residual.as_deref(), Some(&equality(false)));
    }

    #[test]
    fn positional_using_keys_retain_hash_selection_and_each_input_type() {
        let left = RowSchema::with_qualified_types(
            "l",
            vec!["payload".into(), "id".into()],
            vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
        );
        let right = RowSchema::with_qualified_types(
            "r",
            vec!["id".into()],
            vec![Some(ColumnType::BigInteger)],
        );
        let using = uqa_sql::semantics::resolve_join_using(
            Some(&uqa_sql::ast::JoinUsing {
                columns: vec!["id".into()],
                alias: None,
            }),
            false,
            &left,
            &right,
        )
        .unwrap()
        .unwrap();
        let predicate = uqa_sql::semantics::join_using_predicate(&using, &left, &right).unwrap();
        for kind in [
            JoinKind::Inner,
            JoinKind::Left,
            JoinKind::Right,
            JoinKind::Full,
        ] {
            let keys = join_keys(
                kind,
                Some(&predicate),
                &left,
                &right,
                &[],
                &Types,
                &Types,
                &ProductionControl::uncontrolled(),
            )
            .unwrap()
            .expect("USING equality remains hash-joinable");
            assert_eq!(keys.left.len(), 1);
            assert_eq!(keys.right.len(), 1);
            assert!(keys.residual.is_none());
            assert_eq!(
                crate::scalar_type_with_resolver(&keys.left[0], &left, &[], &Types).unwrap(),
                Some(ColumnType::BigInteger)
            );
            assert_eq!(
                crate::scalar_type_with_resolver(&keys.right[0], &right, &[], &Types).unwrap(),
                Some(ColumnType::BigInteger)
            );
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
            &ProductionControl::uncontrolled(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            selected.left.as_slice(),
            [ScalarExpr::qualified_column("l", "v")]
        );
        assert_eq!(
            selected.right.as_slice(),
            [ScalarExpr::qualified_column("r", "v")]
        );
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
            &ProductionControl::uncontrolled(),
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("0A000"));
    }
    #[test]
    fn hash_key_binding_admits_each_operand_and_releases_rejected_candidates() {
        use uqa_core::{memory::MemoryBudget, CancellationToken};
        let payload = "x".repeat(4 << 20);
        let uqa_sql::Statement::Select(mut select) =
            uqa_sql::compile(&format!("SELECT ('{payload}'::text || l.v)=r.v"))
                .unwrap()
                .remove(0)
        else {
            panic!("scalar SELECT")
        };
        let predicate =
            uqa_sql::plan::ExpressionPlan::lower(select.projections.remove(0).expr).scalar;
        let left = schema("l", Some(ColumnType::Text));
        let right = schema("r", Some(ColumnType::Text));
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let select = || {
            join_keys(
                JoinKind::Inner,
                Some(&predicate),
                &left,
                &right,
                &[],
                &Types,
                &Types,
                &control,
            )
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut result = None;
        let allocation = allocation_counter::measure(|| result = Some(select()));
        assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
        assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(select().err().unwrap().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        let keys = select().unwrap().unwrap();
        assert_eq!(keys.left.len(), 1);
        assert_eq!(keys.right.len(), 1);
        assert!(keys.residual.is_none());
        assert!(memory.used() >= payload.len());
        assert_eq!(
            memory.used(),
            keys.left.reserved_bytes() + keys.right.reserved_bytes()
        );
        drop(keys);
        assert_eq!(memory.used(), 0);
        let non_hashable = ScalarExpr::Binary {
            op: BinaryOp::NotEqual,
            lhs: Box::new(ScalarExpr::qualified_column("l", "v")),
            rhs: Box::new(ScalarExpr::Literal(uqa_core::Value::Str(payload))),
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let allocation = allocation_counter::measure(|| {
            assert!(join_keys(
                JoinKind::Inner,
                Some(&non_hashable),
                &left,
                &right,
                &[],
                &Types,
                &Types,
                &control,
            )
            .unwrap()
            .is_none());
        });
        assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(memory.used(), occupied.bytes());
    }
}
