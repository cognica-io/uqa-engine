//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn json_operator_selection_retains_only_selected_input_types() {
    let path = ColumnType::Array(Box::new(ColumnType::Varchar(None)));
    for limit in [0, 32, 128, 512, 4096, 65536] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        match named_binary_operator_types_with_control(
            "#>",
            Some(&ColumnType::JsonB),
            Some(&path),
            &control,
        ) {
            Ok(output) => {
                assert_eq!(
                    *output,
                    [
                        ColumnType::JsonB,
                        ColumnType::Array(Box::new(ColumnType::Text)),
                        ColumnType::JsonB
                    ]
                );
                assert_eq!(output.reserved_bytes(), size_of::<ColumnType>());
                assert_eq!(budget.used(), output.reserved_bytes());
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = named_binary_operator_types_with_control(
            "->",
            Some(&ColumnType::JsonB),
            None,
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn controlled_operator_types_preserve_exact_unknown_and_cross_type_selection() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let cases = [
        (
            BinaryOp::Add,
            Some(ColumnType::Integer),
            None,
            [
                ColumnType::Integer,
                ColumnType::Integer,
                ColumnType::Integer,
            ],
        ),
        (
            BinaryOp::Add,
            Some(ColumnType::SmallInteger),
            Some(ColumnType::BigInteger),
            [
                ColumnType::SmallInteger,
                ColumnType::BigInteger,
                ColumnType::BigInteger,
            ],
        ),
        (
            BinaryOp::Subtract,
            Some(ColumnType::Date),
            Some(ColumnType::Date),
            [ColumnType::Date, ColumnType::Date, ColumnType::Integer],
        ),
        (
            BinaryOp::Equal,
            Some(ColumnType::Text),
            Some(ColumnType::Text),
            [ColumnType::Text, ColumnType::Text, ColumnType::Boolean],
        ),
    ];
    for (op, left, right, expected) in cases {
        let output =
            binary_operator_types_with_control(op, left.as_ref(), right.as_ref(), &control)
                .unwrap();
        assert_eq!(*output, expected);
        assert_eq!(
            binary_operator_types(op, left.as_ref(), right.as_ref()).unwrap(),
            expected
        );
        assert_eq!(output.reserved_bytes(), 0);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn polymorphic_operator_outputs_keep_only_their_nested_type_leases() {
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let input = ColumnType::Array(Box::new(ColumnType::Varchar(Some(12))));
    let output =
        binary_operator_types_with_control(BinaryOp::Equal, Some(&input), Some(&input), &control)
            .unwrap();
    assert_eq!(
        *output,
        [
            ColumnType::Array(Box::new(ColumnType::Varchar(None))),
            ColumnType::Array(Box::new(ColumnType::Varchar(None))),
            ColumnType::Boolean
        ]
    );
    assert_eq!(output.reserved_bytes(), 2 * size_of::<ColumnType>());
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn operator_workspace_failure_is_not_rewritten_as_undefined_operator() {
    for limit in [0, 32, 128, 512, 4096, 65536] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        match binary_operator_types_with_control(
            BinaryOp::Add,
            Some(&ColumnType::Integer),
            Some(&ColumnType::Integer),
            &control,
        ) {
            Ok(output) => {
                assert_eq!(output[2], ColumnType::Integer);
                drop(output);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(1 << 20);
    let cancellation = CancellationToken::new();
    let error = binary_operator_types_with_control(
        BinaryOp::Add,
        Some(&ColumnType::Boolean),
        Some(&ColumnType::Date),
        &ProductionControl::new(&budget, &cancellation, &cancellation),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn either_cancellation_stops_operator_type_production_without_retention() {
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = binary_operator_types_with_control(
            BinaryOp::Add,
            Some(&ColumnType::Integer),
            Some(&ColumnType::Integer),
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn exact_builtin_operator_selection_needs_no_candidate_workspace() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let mut checked = 0;
    for &(name, left, right, result, _, _) in SIGNATURES {
        let scalar = |name| {
            (name != "anyarray")
                .then(|| crate::catalog::type_metadata::builtin_scalar_type_by_name(name))
                .flatten()
        };
        let (Some(left), Some(right), Some(result)) = (scalar(left), scalar(right), scalar(result))
        else {
            continue;
        };
        let selected =
            named_binary_operator_types_with_control(name, Some(left), Some(right), &control)
                .unwrap_or_else(|error| panic!("{left:?} {name} {right:?}: {error}"));
        assert_eq!(&*selected, &[left.clone(), right.clone(), result.clone()]);
        assert_eq!(selected.reserved_bytes(), 0);
        assert_eq!(budget.used(), 0);
        checked += 1;
    }
    assert!(
        checked > 100,
        "the implemented builtin operator catalog must be covered"
    );
}

#[test]
fn containment_operator_identities_and_bounded_types_match_postgresql() {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("containment.json")).unwrap();
    let concrete = |name: &str| {
        ColumnType::from_sql_name(match name {
            "anyarray" => "integer[]",
            "anyelement" => "integer",
            "anyrange" => "int4range",
            "anymultirange" => "int4multirange",
            other => other,
        })
        .unwrap()
    };
    for row in reference["operators"].as_array().unwrap() {
        let operator = row[0].as_str().unwrap();
        let left = concrete(row[1].as_str().unwrap());
        let right = concrete(row[2].as_str().unwrap());
        let selected =
            crate::type_resolution::named_binary_operator_catalog_entry(operator, [&left, &right])
                .unwrap();
        assert_eq!(selected.oid, row[3].as_i64().unwrap(), "{row}");
        assert_eq!(selected.function_oid, row[4].as_i64().unwrap(), "{row}");
        let mut succeeded = false;
        for limit in [0, 32, 4096, 65536] {
            let budget = MemoryBudget::new(limit);
            let cancellation = CancellationToken::new();
            let control = ProductionControl::new(&budget, &cancellation, &cancellation);
            match named_binary_operator_types_with_control(
                operator,
                Some(&left),
                Some(&right),
                &control,
            ) {
                Ok(types) => {
                    assert_eq!(
                        *types,
                        [left.clone(), right.clone(), ColumnType::Boolean],
                        "{row}"
                    );
                    assert_eq!(budget.used(), types.reserved_bytes());
                    succeeded = true;
                }
                Err(error) => assert_eq!(error.sqlstate(), Some("53200"), "{row}: {error}"),
            }
            assert_eq!(budget.used(), 0);
        }
        assert!(succeeded, "{row}");
    }
    for (left, right) in [
        ("int4range", "bigint"),
        ("int4range", "int8range"),
        ("int4range", "int8multirange"),
    ] {
        assert!(
            named_binary_operator_types("@>", Some(&concrete(left)), Some(&concrete(right)))
                .is_err()
        );
    }
}

#[test]
fn retained_builtin_selections_equal_fresh_selections() {
    let cache = BuiltinSelections::default();
    let numeric = ColumnType::Numeric {
        precision: Some(15),
        scale: Some(2),
    };
    let cases = [
        ("*", Some(numeric.clone()), Some(ColumnType::Integer)),
        ("-", Some(ColumnType::Integer), Some(numeric.clone())),
        (
            "=",
            Some(ColumnType::Bpchar),
            Some(ColumnType::Character(10)),
        ),
        ("<", Some(ColumnType::Date), None),
        ("+", None, Some(ColumnType::BigInteger)),
        (
            "||",
            Some(ColumnType::Text),
            Some(ColumnType::Varchar(Some(5))),
        ),
    ];
    let control = ProductionControl::uncontrolled();
    for (name, left, right) in &cases {
        let fresh =
            select_binary_operator_types(name, left.as_ref(), right.as_ref(), &control).unwrap();
        for _ in 0..3 {
            let selected =
                select_with_cache(name, left.as_ref(), right.as_ref(), &control, &cache).unwrap();
            assert_eq!(*selected, *fresh, "{left:?} {name} {right:?}");
        }
        let key = selection_key(name, left.as_ref(), right.as_ref()).unwrap();
        assert_eq!(
            cache.read().unwrap().get(&key),
            Some(&*fresh),
            "{left:?} {name} {right:?}"
        );
    }
    let domain = ColumnType::Domain {
        schema: "public".into(),
        name: "positive".into(),
        oid: 16_384,
        array_oid: None,
        collation_oid: None,
        base: Box::new(ColumnType::Integer),
    };
    assert!(selection_key("+", Some(&domain), Some(&ColumnType::Integer)).is_none());
}

#[test]
fn fresh_builtin_selections_stay_below_the_retained_selection_allowance() {
    let operands = [
        None,
        Some(ColumnType::Boolean),
        Some(ColumnType::SmallInteger),
        Some(ColumnType::Integer),
        Some(ColumnType::BigInteger),
        Some(ColumnType::Real),
        Some(ColumnType::DoublePrecision),
        Some(ColumnType::Numeric {
            precision: Some(15),
            scale: Some(2),
        }),
        Some(ColumnType::Text),
        Some(ColumnType::Varchar(Some(25))),
        Some(ColumnType::Character(10)),
        Some(ColumnType::Date),
        Some(ColumnType::Timestamp),
        Some(ColumnType::TimestampTz),
        Some(ColumnType::Interval),
        Some(ColumnType::Bytea),
        Some(ColumnType::JsonB),
    ];
    let cancellation = CancellationToken::new();
    let mut names: Vec<&str> = SIGNATURES.iter().map(|signature| signature.0).collect();
    names.sort_unstable();
    names.dedup();
    for name in names {
        for left in &operands {
            for right in &operands {
                let budget = MemoryBudget::new(RETAINED_SELECTION_ALLOWANCE);
                let control = ProductionControl::new(&budget, &cancellation, &cancellation);
                let selected =
                    select_binary_operator_types(name, left.as_ref(), right.as_ref(), &control);
                assert_ne!(
                    selected.as_ref().err().and_then(SQLError::sqlstate),
                    Some("53200"),
                    "{left:?} {name} {right:?}"
                );
                assert!(
                    budget.peak() < RETAINED_SELECTION_ALLOWANCE,
                    "{left:?} {name} {right:?}"
                );
            }
        }
    }
}

#[test]
fn modifier_variants_share_one_retained_selection_and_the_retention_stays_bounded() {
    let cache = BuiltinSelections::default();
    let select = |name: &str, left: Option<&ColumnType>, right: Option<&ColumnType>| {
        select_with_cache(
            name,
            left,
            right,
            &ProductionControl::uncontrolled(),
            &cache,
        )
        .unwrap()
        .into_uncontrolled()
        .unwrap()
    };
    let contains = |name: &str, left: Option<ColumnType>, right: Option<ColumnType>| {
        cache
            .read()
            .unwrap()
            .contains_key(&(name.into(), OperandType(left), OperandType(right)))
    };
    let numeric = |precision, scale| ColumnType::Numeric {
        precision: Some(precision),
        scale: Some(scale),
    };
    let first = select("*", Some(&numeric(15, 2)), Some(&ColumnType::Integer));
    let second = select("*", Some(&numeric(7, 3)), Some(&ColumnType::Integer));
    assert_eq!(first, second);
    assert!(contains(
        "*",
        Some(ColumnType::Numeric {
            precision: None,
            scale: None,
        }),
        Some(ColumnType::Integer),
    ));
    assert!(!contains(
        "*",
        Some(numeric(15, 2)),
        Some(ColumnType::Integer)
    ));
    assert!(!contains(
        "*",
        Some(numeric(7, 3)),
        Some(ColumnType::Integer)
    ));
    let text = select(
        "||",
        Some(&ColumnType::Varchar(Some(12))),
        Some(&ColumnType::Varchar(Some(3))),
    );
    let plain_text = select(
        "||",
        Some(&ColumnType::Varchar(None)),
        Some(&ColumnType::Varchar(None)),
    );
    assert_eq!(text, plain_text);
    assert!(contains(
        "||",
        Some(ColumnType::Varchar(None)),
        Some(ColumnType::Varchar(None)),
    ));
    assert!(!contains(
        "||",
        Some(ColumnType::Varchar(Some(12))),
        Some(ColumnType::Varchar(Some(3))),
    ));
    // The capacity fixture owns its map so its synthetic keys cannot evict
    // selections another test is inspecting in the process-wide cache.
    let mut selections = HashMap::new();
    let types = [
        ColumnType::Integer,
        ColumnType::Integer,
        ColumnType::Integer,
    ];
    for index in 0..RETAINED_SELECTIONS {
        insert_selection(
            &mut selections,
            (
                format!("bounded_operator_{index}").into(),
                OperandType(None),
                OperandType(None),
            ),
            &types,
        );
        assert_eq!(selections.len(), index + 1);
    }
    assert_eq!(selections.len(), RETAINED_SELECTIONS);
    let last = (
        "after_capacity".into(),
        OperandType(None),
        OperandType(None),
    );
    insert_selection(&mut selections, last.clone(), &types);
    assert_eq!(selections.len(), 1);
    assert_eq!(selections.get(&last), Some(&types));
    assert!(!selections.contains_key(&(
        "bounded_operator_0".into(),
        OperandType(None),
        OperandType(None),
    )));
}
