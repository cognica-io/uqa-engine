//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

mod compatibility;
mod retention;

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<FunctionBinding>(text);
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = FunctionBinding::from_stored_json_with_control(text, &control);
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?} / {expected:?}"
    );
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
        assert_eq!(memory.used(), actual.reserved_bytes());
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

fn binding(fields: &str) -> String {
    let suffix = if fields.is_empty() {
        String::new()
    } else {
        format!(",{fields}")
    };
    format!(r#"{{"name":"schema.\\\"call","argument_types":["integer","text"]{suffix}}}"#)
}

pub(super) const COMPLETE: &str = r#"{
"object_id":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,255],
"name":"schema.\"call","argument_types":["integer","text"],"builtin":false,
"dispatch":{"Pattern":{"operator":"Like","oid":1209}},
"invocation":{"argument_positions":[1,0],"argument_targets":["bigint","text"],"argument_sources":["integer",null],"parameter_types":["text","bigint"],"return_type":"text","variadic_mode":{"Expanded":{"parameter_index":1}}},
"composite_field":{"type_oid":40000,"number":-12,"result_type":{"Array":{"Composite":{"schema":"s","name":"original","oid":40001,"array_oid":40002,"relation_oid":50000}}},"collation_oid":100,"dropped":true,"changed_type":{"Domain":{"schema":"s","name":"changed","oid":41000,"array_oid":41001,"collation_oid":100,"base":"Text"}}},
"resolution_error":{"Operator":{"sqlstate":"42883","message":"original error"}},"input_collation":100
}"#;

const ASSIGNMENT: &str = r#""assignment":{"path":[{"Field":{"type_oid":1234,"number":-3}},{"Subscripts":{"first_argument":2,"dimensions":6,"slice":true,"bounds":{"omitted_lower":255,"omitted_upper":192}}}],"value_argument":4}"#;

#[test]
fn stored_call_input_retains_complete_selected_metadata_and_legacy_defaults() {
    assert!(compare(&binding("")));
    assert!(compare(COMPLETE));
    assert!(compare(&binding(ASSIGNMENT)));
    for value in ["None", "Expanded", "Explicit"] {
        let variadic = if value == "None" {
            r#""None""#.into()
        } else {
            format!(r#"{{"{value}":{{"parameter_index":0}}}}"#)
        };
        assert!(compare(&binding(&format!(
            r#""invocation":{{"argument_positions":[],"argument_targets":[],"parameter_types":[],"variadic_mode":{variadic}}}"#
        ))));
    }
    for error in [
        r#"{"UndefinedFunction":{"signature":"original(integer)"}}"#,
        r#"{"Operator":["42883","message"]}"#,
        "null",
    ] {
        assert!(compare(&binding(&format!(r#""resolution_error":{error}"#))));
    }
    for oid in [0, 100, 950, 951, u32::MAX] {
        compare(&binding(&format!(r#""input_collation":{oid}"#)));
    }
}

#[test]
fn stored_call_input_preserves_all_native_dispatch_shapes() {
    for tag in [
        "NamedArgument",
        "VariadicArgument",
        "ArraySubscripts",
        "ArraySlices",
        "Subscript",
        "Slice",
        "FieldSelect",
        "AssignmentStore",
        "AnyOperator",
        "AllOperator",
        "IsDistinct",
        "DistinctNot",
        "RowDistinct",
        "ValueIsDistinct",
        "BetweenSymmetric",
        "ToBinInt4",
        "ToBinInt8",
        "ToHexInt4",
        "ToHexInt8",
        "ToOctInt4",
        "ToOctInt8",
        "RandomInt4Range",
        "RandomInt8Range",
        "RandomNumericRange",
        "RegexSubstring",
        "SimilarSubstring",
        "ArraySortJson",
        "ArrayConcat",
        "ArrayAppend",
        "ArrayPrepend",
        "MergeAction",
    ] {
        assert!(compare(&binding(&format!(r#""dispatch":"{tag}""#))));
        assert!(compare(&binding(&format!(
            r#""dispatch":{{"{tag}":null}}"#
        ))));
    }
    for (tag, payload) in [
        ("NumericOperator", r#""BitXor""#),
        ("BooleanTest", r#""IsNotUnknown""#),
        (
            "ArraySlicesWithBounds",
            r#"{"omitted_lower":255,"omitted_upper":128}"#,
        ),
        ("Pattern", r#"{"operator":"Like","oid":1209}"#),
        ("Pattern", r#"["Like",null]"#),
        ("PatternEscape", r#"{"similar":true}"#),
        ("ValueNullTest", "[false]"),
        ("Collate", "{}"),
        ("Concat", r#"{"oid":44}"#),
        ("TypeOf", r#"{"type_oid":23}"#),
        (
            "CollationFor",
            r#"{"collation_oid":100,"noncollatable_type_oid":23}"#,
        ),
        ("JsonPath", r#"{"as_text":true,"jsonb":false}"#),
        ("JsonExtract", "[false,true]"),
        (
            "Range",
            r#"{"operation":"ElementContainedBy","subtype":"TimestampTz","multirange":true}"#,
        ),
        ("Enum", r#"{"operation":"First","type_oid":40000}"#),
    ] {
        assert!(compare(&binding(&format!(
            r#""dispatch":{{"{tag}":{payload}}}"#
        ))));
    }
}
