//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::catalog_types::Catalog;
use super::*;
use crate::optimizer::scalar::constants::composites::{fold_composite_constructor, fold_field};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::ast::{CompositeFieldBinding, CompositeRowBinding, FunctionDispatch};

mod failures;

#[test]
fn retained_composite_values_preserve_catalog_dependent_folding_eligibility() {
    let budget = MemoryBudget::new(65536);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let composite = fold_composite_constructor(constructor(64), Some(&Catalog), &control).unwrap();
    let call = field(composite.clone(), "b", true);
    let selected = fold_field(&call, Some(&Catalog), &control)
        .unwrap()
        .unwrap();
    let mut config = crate::OptimizerConfig::new(|_, _, _| panic!("catalog-dependent evaluation"));
    config.production = control;
    for literal in [composite, selected] {
        let source = ScalarExpr::Array(vec![literal]);
        let expected = source.clone();
        let output = fold_selected_literal(
            source,
            |_, _| panic!("physical literals require their catalog consumer"),
            None,
            Some(&Catalog),
            None,
            None,
            &config,
        )
        .unwrap();
        assert_eq!(output, expected);
    }
    drop(call);
    assert_eq!(budget.used(), 0);
}

fn composite_type() -> ColumnType {
    ColumnType::Composite(uqa_sql::ast::CompositeTypeReference {
        schema: "public".into(),
        name: "pair".into(),
        oid: 20_001,
        array_oid: 20_003,
        relation_oid: 20_002,
    })
}

fn constructor(bytes: usize) -> ScalarExpr {
    let ty = composite_type();
    ScalarExpr::CompositeRow {
        items: vec![
            ScalarExpr::Literal(Value::Int(7)),
            ScalarExpr::Literal(Value::Str("x".repeat(bytes))),
        ],
        binding: CompositeRowBinding {
            layout: None,
            ty: ty.catalog_name(),
            attributes: vec![1, 2],
            argument_types: Some(vec![ColumnType::Integer, ColumnType::Text]),
        },
        bound_type: Some(ty),
    }
}

fn field(base: ScalarExpr, name: &str, bound: bool) -> ScalarExpr {
    let mut call = bound_call(
        "field selection",
        vec![base, ScalarExpr::Literal(Value::Str(name.into()))],
    );
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut call
    else {
        panic!("field")
    };
    binding.dispatch = Some(FunctionDispatch::FieldSelect);
    if bound {
        binding.set_composite_field(CompositeFieldBinding {
            type_oid: 20_001,
            number: if name == "a" { 1 } else { 2 },
            result_type: if name == "a" {
                ColumnType::Integer
            } else {
                ColumnType::Text
            },
            collation_oid: Some(950),
            dropped: false,
            changed_type: None,
        });
    }
    call
}

fn materialize(expression: &ScalarExpr) -> ScalarExpr {
    match expression {
        ScalarExpr::RetainedLiteral(literal) => literal.materialize().unwrap(),
        expression => expression.clone(),
    }
}

#[test]
fn composite_constructor_and_fields_preserve_values_types_collations_and_last_owner() {
    let ordinary = ProductionControl::uncontrolled();
    let expected =
        fold_composite_constructor(constructor(16384), Some(&Catalog), &ordinary).unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = fold_composite_constructor(constructor(16384), Some(&Catalog), &control).unwrap();
    assert_eq!(materialize(&output), expected);
    let retained = budget.used();
    assert!(retained > 16384);
    let mut copies = None;
    let allocated = allocation_counter::measure(|| {
        copies = Some(std::array::from_fn::<_, 32, _>(|_| output.clone()));
    });
    assert_eq!(allocated.count_total, 0);
    let copies = copies.unwrap();
    drop(output);
    assert_eq!(budget.used(), retained);
    for name in ["a", "b"] {
        for bound in [false, true] {
            for cast in [false, true] {
                let wrap = |expression| {
                    if cast {
                        ScalarExpr::Cast {
                            expr: Box::new(expression),
                            ty: composite_type().catalog_name(),
                            implicit: false,
                        }
                    } else {
                        expression
                    }
                };
                let expected_call = field(wrap(expected.clone()), name, bound);
                let actual_call = field(wrap(copies[0].clone()), name, bound);
                let expected = fold_field(&expected_call, Some(&Catalog), &ordinary)
                    .unwrap()
                    .unwrap();
                let actual = fold_field(&actual_call, Some(&Catalog), &control)
                    .unwrap()
                    .unwrap();
                assert_eq!(materialize(&actual), expected);
                assert!(budget.used() > retained);
                drop(actual);
                assert_eq!(budget.used(), retained);
            }
        }
    }
    drop(copies);
    assert_eq!(budget.used(), 0);
}

#[test]
fn composite_constant_metadata_rejects_large_names_before_output_copy() {
    let mut source = constructor(64);
    let ScalarExpr::CompositeRow {
        bound_type: Some(ColumnType::Composite(reference)),
        ..
    } = &mut source
    else {
        panic!("constructor")
    };
    reference.schema = "s".repeat(4 << 20);
    let budget = MemoryBudget::new(256);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        let error = fold_composite_constructor(source, Some(&Catalog), &control).unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 4096, "{measured:?}");
    assert_eq!(budget.used(), 0);

    let source = fold_composite_constructor(
        constructor(64),
        Some(&Catalog),
        &ProductionControl::uncontrolled(),
    )
    .unwrap();
    let mut call = field(source, "b", true);
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut call
    else {
        panic!("field")
    };
    binding.composite_field_mut().unwrap().result_type = ColumnType::Domain {
        schema: "s".repeat(4 << 20),
        name: "text_domain".into(),
        oid: 30_001,
        array_oid: Some(30_002),
        base: Box::new(ColumnType::Text),
        collation_oid: None,
    };
    let budget = MemoryBudget::new(8192);
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        let error = fold_field(&call, Some(&Catalog), &control)
            .unwrap()
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 16384, "{measured:?}");
    assert_eq!(measured.bytes_current, 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn composite_field_copy_validates_selected_bytes_before_preserving_original_tuple() {
    let value = Value::Record(uqa_core::RecordValue::from_parts(
        vec![
            ("a".into(), Value::Int(7)),
            (
                "b".into(),
                Value::Datum(uqa_core::DatumValue::new(
                    25,
                    0,
                    vec![2, 0, 0, 0, 3, 0, 0, 0],
                )),
            ),
        ],
        Some(20_001),
    ));
    let ty = composite_type();
    let source = ScalarExpr::TypedLiteral {
        composite_source: Some(Box::new(
            uqa_sql::expr::composites::CompositeConstantSource {
                value: value.clone(),
                descriptors: Vec::new(),
            },
        )),
        value,
        ty: ty.catalog_name(),
        bound_type: Some(ty),
        parameter_index: None,
        collation_oid: None,
    };
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let safe = field(source.clone(), "a", true);
    assert!(fold_field(&safe, Some(&Catalog), &control).is_none());
    assert_eq!(budget.used(), 0);
    let invalid = field(source, "b", true);
    let error = fold_field(&invalid, Some(&Catalog), &control)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX001"));
    assert_eq!(error.to_string(), "compressed pglz data is corrupt");
    assert_eq!(budget.used(), 0);
}
