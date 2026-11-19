//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{RangeFunctionOperation, RangeSubtype};

fn marked(name: String) -> CreateFunction {
    let mut function = declaration(Vec::new());
    let mut expression = legacy_expression();
    let Expr::Func {
        name: target,
        binding,
        ..
    } = &mut expression
    else {
        unreachable!()
    };
    *target = name;
    *binding = None;
    function.params[0].default = Some(expression);
    function
}

#[test]
fn retained_routine_dispatch_observation_bounds_legacy_range_marker_spelling() {
    let operations = [
        ("lower", RangeFunctionOperation::Lower),
        ("upper", RangeFunctionOperation::Upper),
        ("isempty", RangeFunctionOperation::IsEmpty),
        ("lower_inc", RangeFunctionOperation::LowerInclusive),
        ("upper_inc", RangeFunctionOperation::UpperInclusive),
        ("lower_inf", RangeFunctionOperation::LowerInfinite),
        ("upper_inf", RangeFunctionOperation::UpperInfinite),
        ("merge", RangeFunctionOperation::Merge),
        ("multirange", RangeFunctionOperation::Multirange),
        ("overlap", RangeFunctionOperation::Overlap),
        ("contains", RangeFunctionOperation::Contains),
        ("contained_by", RangeFunctionOperation::ContainedBy),
        ("adjacent", RangeFunctionOperation::Adjacent),
    ];
    for (range, multirange, subtype) in [
        ("int4range", "int4multirange", RangeSubtype::Integer),
        ("int8range", "int8multirange", RangeSubtype::BigInteger),
        ("numrange", "nummultirange", RangeSubtype::Numeric),
        ("daterange", "datemultirange", RangeSubtype::Date),
        ("tsrange", "tsmultirange", RangeSubtype::Timestamp),
        ("tstzrange", "tstzmultirange", RangeSubtype::TimestampTz),
    ] {
        for (type_name, multirange) in [(range, false), (multirange, true)] {
            for (spelling, operation) in operations {
                for length in [0, 1, 4096, 8193] {
                    let name = format!("__range_{spelling}{}{type_name}", "_".repeat(length));
                    assert_eq!(
                        FunctionDispatch::from_legacy_serialized_name(&name),
                        Some(FunctionDispatch::Range {
                            operation,
                            subtype,
                            multirange
                        })
                    );
                    compare(marked(name), true);
                }
            }
        }
    }
    for name in [
        format!("__range_unknown{}int4range", "_".repeat(16_385)),
        format!("__range_{}int4range", "_".repeat(16_385)),
        format!("__range_lower{}int4range_", "_".repeat(16_385)),
        format!("__range_한🙂{}int4range", "_".repeat(16_385)),
    ] {
        assert_eq!(FunctionDispatch::from_legacy_serialized_name(&name), None);
        compare(marked(name), false);
    }
    verify_long_marker_retirement();
}

fn verify_long_marker_retirement() {
    let function = Arc::new(marked(format!(
        "__range_lower{}int4range",
        "_".repeat(131_073)
    )));
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (_, polls) = complete(
        &mut decoder(Arc::clone(&function), &memory, &token, &token).unwrap(),
        1,
    )
    .unwrap();
    assert!(polls >= 33, "every 4096 marker bytes must consume work");
    assert_eq!(memory.used(), 0);
    for pending in 0..polls - 1 {
        for failure in ["original", "invoking", "drop"] {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut input = decoder(Arc::clone(&function), &memory, &original, &invoking).unwrap();
            for _ in 0..pending {
                assert!(advance(&mut input, 1).unwrap().is_none());
            }
            match failure {
                "original" => original.cancel(),
                "invoking" => invoking.cancel(),
                "drop" => {}
                _ => unreachable!(),
            }
            if failure != "drop" {
                assert!(matches!(
                    advance(&mut input, 1),
                    Err(ImageIoError::Cancelled(_))
                ));
            }
            drop(input);
            assert_eq!(memory.used(), 0);
        }
    }
}
