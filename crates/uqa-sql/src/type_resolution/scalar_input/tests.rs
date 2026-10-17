//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::RowSchema;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn row_field_collations_use_analyzed_inputs_and_observe_cancellation() {
    let schema = RowSchema::with_types(
        vec!["selected".into(), "indeterminate".into()],
        vec![Some(ColumnType::Text); 2],
    )
    .with_column_collations([Some(950), Some(0)]);
    let expressions = [
        (ScalarExpr::Column("selected".into()), 950),
        (ScalarExpr::Position(1), 0),
        (ScalarExpr::Literal(Value::Null), 0),
        (
            ScalarExpr::Collated {
                expr: Box::new(ScalarExpr::Literal(Value::Str("B".into()))),
                ty: ColumnType::Text,
                collation: crate::ir::ResultCollation::new(
                    crate::catalog::collations::derivation::Derivation::Explicit(963),
                    100,
                ),
            },
            963,
        ),
    ];
    for cancel in [None, Some(false), Some(true)] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if let Some(owner) = cancel {
            if owner {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        for (expression, expected) in &expressions {
            let field =
                scalar_record_field_type_with_control(expression, &schema, &[], None, &control);
            if cancel.is_some() {
                assert_eq!(field.unwrap_err().sqlstate(), Some("57014"));
            } else {
                assert_eq!(field.unwrap().unwrap().collation_oid, Some(*expected));
            }
            assert_eq!(budget.used(), 0);
        }
    }
}

fn domain(base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        collation_oid: None,
        schema: "app".into(),
        name: "bounded".into(),
        oid: 91_001,
        array_oid: None,
        base: Box::new(base),
    }
}

#[test]
fn array_constructor_common_type_includes_unknown_inputs() {
    let schema = RowSchema::with_types(vec!["domain".into()], vec![Some(domain(ColumnType::Text))]);
    let known = ScalarExpr::Column("domain".into());
    let unknown = ScalarExpr::Literal(Value::Null);
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(2048);
    let control = ProductionControl::new(&budget, &token, &token);
    for (items, oid) in [
        (vec![known.clone(), known.clone()], 91_001),
        (vec![known.clone(), unknown.clone()], 25),
        (vec![unknown, known], 25),
    ] {
        assert_eq!(
            scalar_array_element_oid_with_control(&items, &schema, &[], None, &control).unwrap(),
            Some(oid)
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn constructor_result_inference_retains_catalog_identity_without_a_resolver() {
    let ty = ColumnType::Composite(crate::ast::CompositeTypeReference {
        schema: "app".repeat(100),
        name: "pair".repeat(100),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    });
    let row = ScalarExpr::CompositeRow {
        items: vec![ScalarExpr::Literal(Value::Int(1))],
        binding: crate::ast::CompositeRowBinding {
            layout: None,
            argument_types: None,
            ty: "composite#20001".into(),
            attributes: vec![1],
        },
        bound_type: Some(ty.clone()),
    };
    let token = CancellationToken::new();
    let schema = RowSchema::default();
    let budget = MemoryBudget::new(2048);
    let control = ProductionControl::new(&budget, &token, &token);
    let inferred = crate::scalar_type_with_control(&row, &schema, &[], &control)
        .unwrap()
        .unwrap();
    assert_eq!(*inferred, ty);
    assert!(budget.used() >= 700);
    drop(inferred);
    assert_eq!(budget.used(), 0);
    let small = MemoryBudget::new(32);
    let control = ProductionControl::new(&small, &token, &token);
    assert_eq!(
        crate::scalar_type_with_control(&row, &schema, &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    token.cancel();
    assert_eq!(
        crate::scalar_type_with_control(&row, &schema, &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
}

#[test]
fn constructor_cast_sources_keep_their_catalog_identity_under_the_output_budget() {
    let budget = MemoryBudget::new(128);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let row = ScalarExpr::CompositeRow {
        bound_type: None,
        items: vec![ScalarExpr::Literal(Value::Int(1))],
        binding: crate::ast::CompositeRowBinding {
            layout: None,
            argument_types: None,
            ty: "composite#20001".into(),
            attributes: vec![1],
        },
    };
    let name =
        scalar_cast_source_type_name_with_control(&row, &RowSchema::default(), &[], &control)
            .unwrap()
            .unwrap();
    assert_eq!(&*name, "composite#20001");
    assert_eq!(budget.used(), name.reserved_bytes());
    drop(name);
    assert_eq!(budget.used(), 0);
}

#[test]
fn record_cast_names_preserve_named_sources_but_not_anonymous_nulls() {
    let composite = ColumnType::Composite(crate::ast::CompositeTypeReference {
        schema: "app".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_002,
        relation_oid: 20_003,
    });
    let domain = domain(composite.clone());
    let schema = RowSchema::with_types(
        vec!["named".into(), "domain".into()],
        vec![Some(composite.clone()), Some(domain.clone())],
    );
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8192);
    let control = ProductionControl::new(&budget, &token, &token);
    for target in ["record", "pg_catalog.record"] {
        for (source, expected_source, expected_operand) in [
            (
                ScalarExpr::Position(0),
                composite.catalog_name(),
                composite.catalog_name(),
            ),
            (
                ScalarExpr::Position(1),
                domain.catalog_name(),
                composite.catalog_name(),
            ),
            (
                ScalarExpr::Literal(Value::Null),
                target.into(),
                target.into(),
            ),
        ] {
            let expression = ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(source),
                ty: target.into(),
            };
            for (preserve_domain, expected) in [(true, expected_source), (false, expected_operand)]
            {
                let name =
                    operand_type_name(&expression, &schema, &[], None, &control, preserve_domain)
                        .unwrap()
                        .unwrap();
                assert_eq!(&*name, &expected);
                assert_eq!(budget.used(), name.reserved_bytes());
                drop(name);
                assert_eq!(budget.used(), 0);
            }
        }
    }
    let typed_null = ScalarExpr::Cast {
        implicit: false,
        expr: Box::new(ScalarExpr::Literal(Value::Null)),
        ty: "record".into(),
    };
    assert_eq!(
        scalar_array_element_oid_with_control(
            std::slice::from_ref(&typed_null),
            &schema,
            &[],
            None,
            &control
        )
        .unwrap(),
        Some(2249)
    );
    assert_eq!(budget.used(), 0);
    token.cancel();
    assert_eq!(
        scalar_array_element_oid_with_control(&[typed_null], &schema, &[], None, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn array_constructor_type_selection_resolves_aliases_and_checks_catalog_absence() {
    struct Catalog<'a> {
        ty: ColumnType,
        token: &'a CancellationToken,
    }
    impl crate::expr::EngineHook for Catalog<'_> {
        fn nextval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn currval(&self, _: &str) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
            unreachable!()
        }
        fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
            if matches!(name, "composite#20001" | "app.pair") {
                Ok(Some(self.ty.clone()))
            } else {
                self.token.cancel();
                Ok(None)
            }
        }
    }
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8192);
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::default();
    let catalog = Catalog {
        ty: ColumnType::Composite(crate::ast::CompositeTypeReference {
            schema: "app".into(),
            name: "pair".into(),
            oid: 20001,
            array_oid: 20002,
            relation_oid: 20003,
        }),
        token: &token,
    };
    let items = ["composite#20001", "app.pair"].map(|ty| ScalarExpr::Cast {
        implicit: false,
        expr: Box::new(ScalarExpr::Literal(Value::Null)),
        ty: ty.into(),
    });
    assert_eq!(
        scalar_array_element_oid_with_control(&items, &schema, &[], Some(&catalog), &control)
            .unwrap(),
        Some(20001)
    );
    assert_eq!(budget.used(), 0);
    let absent = ScalarExpr::Cast {
        implicit: false,
        expr: Box::new(ScalarExpr::Literal(Value::Null)),
        ty: "absent".into(),
    };
    assert_eq!(
        scalar_array_element_oid_with_control(&[absent], &schema, &[], Some(&catalog), &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_vector_cast_sources_retain_domain_identity_without_changing_operators() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let ty = domain(ColumnType::Int2Vector);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
    let params = [SQLParam::typed_scalar(Value::Null, ty.clone())];
    for expression in [
        ScalarExpr::Column("value".into()),
        ScalarExpr::Position(0),
        ScalarExpr::Param(1),
        ScalarExpr::TypedLiteral {
            collation_oid: None,
            composite_source: None,
            value: Value::Null,
            ty: "ignored_spelling".into(),
            bound_type: Some(ty.clone()),
            parameter_index: None,
        },
    ] {
        let name =
            scalar_cast_source_type_name_with_control(&expression, &schema, &params, &control)
                .unwrap()
                .unwrap();
        assert_eq!(&*name, &ty.catalog_name());
        drop(name);
        let name = scalar_operand_type_name_with_control(&expression, &schema, &params, &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*name, "pg_catalog.\"int2vector\"");
        drop(name);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn operand_names_keep_bound_type_and_unknown_literal_precedence() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let ty = domain(domain(ColumnType::Varchar(Some(12))));
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty.clone())]);
    for (expression, expected) in [
        (
            ScalarExpr::Column("value".into()),
            "pg_catalog.\"varchar\"(12)",
        ),
        (
            ScalarExpr::TypedLiteral {
                collation_oid: None,
                composite_source: None,
                value: Value::Int(1),
                ty: "ignored_spelling".into(),
                bound_type: Some(domain(ColumnType::SmallInteger)),
                parameter_index: None,
            },
            "pg_catalog.\"int2\"",
        ),
        (
            ScalarExpr::Cast {
                implicit: false,
                expr: Box::new(ScalarExpr::Literal(Value::Int(1))),
                ty: "pg_catalog.int8".into(),
            },
            "pg_catalog.int8",
        ),
        (
            ScalarExpr::Literal(Value::Int(i64::MAX)),
            "pg_catalog.\"int8\"",
        ),
    ] {
        let name = scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
            .unwrap()
            .unwrap();
        assert_eq!(&*name, expected);
        assert_eq!(budget.used(), name.reserved_bytes());
        drop(name);
        assert_eq!(budget.used(), 0);
    }
    for expression in [
        ScalarExpr::Literal(Value::Str("unknown".into())),
        ScalarExpr::Column("missing".into()),
    ] {
        assert!(
            scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
                .unwrap()
                .is_none()
        );
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(schema.column_type(0), Some(&ty));
}

#[test]
fn integer_operand_widths_keep_literal_domain_and_parameter_rules() {
    let budget = MemoryBudget::new(65_536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let schema = RowSchema::with_types(
        vec!["narrow".into()],
        vec![Some(domain(ColumnType::SmallInteger))],
    );
    let narrow = ScalarExpr::Column("narrow".into());
    let params = [SQLParam::typed_scalar(
        Value::Int(1),
        domain(ColumnType::BigInteger),
    )];
    for (right, expected) in [
        (narrow.clone(), Some(IntegerWidth::SmallInt)),
        (
            ScalarExpr::Literal(Value::Int(1)),
            Some(IntegerWidth::Integer),
        ),
        (
            ScalarExpr::Literal(Value::Int(i64::MAX)),
            Some(IntegerWidth::BigInt),
        ),
        (ScalarExpr::Param(1), Some(IntegerWidth::BigInt)),
        (
            ScalarExpr::UnaryMinus(Box::new(ScalarExpr::Binary {
                op: BinaryOp::Add,
                lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
                rhs: Box::new(ScalarExpr::Literal(Value::Int(i64::MAX))),
            })),
            Some(IntegerWidth::BigInt),
        ),
        (ScalarExpr::Literal(Value::Str("unknown".into())), None),
    ] {
        assert_eq!(
            scalar_integer_operation_width_with_control(
                &narrow, &right, &schema, &params, &control
            )
            .unwrap(),
            expected
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn optional_operand_inference_does_not_swallow_allocation_failure() {
    let budget = MemoryBudget::new(1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let held = control.copy_text("previous result").unwrap();
    let retained = budget.used();
    let ty = ColumnType::Domain {
        collation_oid: None,
        schema: "app".into(),
        name: "n".repeat(4096),
        oid: 91_002,
        array_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ty)]);
    let expression = ScalarExpr::Column("value".into());
    assert_eq!(
        scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(
        scalar_integer_operation_width_with_control(
            &expression,
            &expression,
            &schema,
            &[],
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), retained);
    assert_eq!(&*held, "previous result");
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn optional_operand_paths_check_both_cancellation_scopes() {
    let budget = MemoryBudget::new(1024);
    let schema = RowSchema::default();
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let expression = ScalarExpr::Literal(Value::Str("unknown".into()));
        assert_eq!(
            scalar_operand_type_name_with_control(&expression, &schema, &[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            scalar_integer_operation_width_with_control(
                &expression,
                &expression,
                &schema,
                &[],
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
