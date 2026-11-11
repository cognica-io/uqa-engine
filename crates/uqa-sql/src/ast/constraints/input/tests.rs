//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod compatibility;
mod retention;
mod tables;

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<Vec<ColumnDef>>(text);
    let memory = MemoryBudget::new(1 << 26);
    let token = CancellationToken::new();
    let actual = read_stored_columns_json(text, &memory, &token, &token);
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
        assert_eq!(actual.reserved_bytes(), memory.used());
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

const SIMPLE: &str = r#"[{"name":"column","ty":"Text","primary_key":false,"not_null":false}]"#;

fn complete() -> serde_json::Value {
    serde_json::from_str(r#"[{
        "name": "한글_column",
        "ty": {"Domain":{"schema":"s","name":"d","oid":40000,"array_oid":40001,"base":{"Array":"Text"}}},
        "collation_oid":100,"object_id":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],
        "attribute_number":3,"missing_value":[1,"original",null],
        "primary_key":true,"not_null":true,"not_null_explicit":true,"not_null_name":"nn",
        "not_null_identity":{"object_id":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"oid":40002},
        "not_null_validated":false,"not_null_no_inherit":true,"not_null_is_local":false,
        "auto_increment":{
            "kind":"identity_always","sequence":"s.seq","owner":{"table":"s.t","column":"original"},
            "declaration":{
                "name":{"schema":"s","name":"seq"},"persistence":"Unlogged",
                "sequence":{"data_type":"BigInteger","increment":{"Integer":2},"cycle":true,
                    "max_value":{"Text":"99999999999999999999999"},"min_value":"Absent",
                    "start":{"Integer":5},"restart":{"Absent":null},"cache":{"Text":"1e1"},
                    "owned_by":["s","t","original"]},
                "error":{"sqlstate":"22003","message":"deferred original error","detail":"detail","hint":"hint"}
            }
        },
        "unique":true,
        "default":{"Func":{"name":"source_f","binding":{"object_id":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"name":"selected_f","argument_types":["integer"]},"args":[{"Literal":1}],"distinct":false,"order_by":[]}},
        "generated":{"kind":"Stored","expression":{"Binary":{"op":"Add","lhs":{"Column":"x"},"rhs":{"Literal":1}}},"function_dependencies":[{"name":"selected_f","argument_types":["integer"]}]},
        "default_catalog_oid":40003,"check":{"Not":{"IsNull":{"expr":{"Column":"x"},"negated":false}}},
        "check_name":"ck","check_enforced":false,"check_validated":false,"check_no_inherit":true,"check_is_local":false,
        "check_object_id":[2,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"check_catalog_oid":40004,
        "references":{
            "referenced_index":[3,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"referenced_key":"parent_key","name":"fk",
            "object_id":[4,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
            "catalog_identity":{"object_id":[5,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"oid":40005},
            "table":"s.parent","column":"id","on_update":"Cascade","on_delete":"SetNull","match_type":"Full",
            "enforced":false,"validated":false,"deferrable":true,"initially_deferred":true,"period":true,
            "referenced_partitions":[{"partition":[6,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"parent":[7,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"name":"partition_fk","catalog_identity":{"object_id":[8,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"oid":40006},"validated":false}]
        }
    }]"#).unwrap()
}

#[test]
fn stored_columns_json_preserves_complete_native_declarations_and_order() {
    assert!(compare(SIMPLE));
    let column = complete()[0].take();
    for columns in [vec![], vec![column.clone()], vec![column.clone(), column]] {
        assert!(compare(&serde_json::to_string(&columns).unwrap()));
    }
}

#[test]
fn stored_columns_json_preserves_legacy_sequence_and_positional_forms() {
    for source in [
        "null",
        "true",
        "false",
        r#"{"kind":"legacy"}"#,
        r#"{"kind":"serial"}"#,
        r#"["identity_by_default"]"#,
        r#"{"kind":"identity_always","declaration":{"sequence":{}}}"#,
    ] {
        let text = SIMPLE.replace(
            r#""name":"column""#,
            &format!(r#""auto_increment":{source},"name":"column""#),
        );
        assert!(compare(&text));
    }
    for text in [
        r#"[["x","Integer",null,null,null,null,false,false]]"#,
        r#"[["x","Integer"]]"#,
        r#"[{"name":"x","ty":"Integer","primary_key":false,"not_null":false,"references":[null,null,null,null,null,"parent"]}]"#,
        r#"[{"name":"x","ty":"Integer","primary_key":false,"not_null":false,"generated":["Virtual",{"Literal":1}]}]"#,
    ] {
        compare(text);
    }
}

#[test]
fn stored_columns_json_rejects_query_expressions_without_execution() {
    let crate::ast::Statement::Select(select) = crate::compiler::compile("SELECT (SELECT 1)")
        .unwrap()
        .pop()
        .unwrap()
    else {
        panic!("SELECT fixture");
    };
    let expression = serde_json::to_value(&select.projections[0].expr).unwrap();
    for field in ["default", "check", "generated"] {
        let mut columns: serde_json::Value = serde_json::from_str(SIMPLE).unwrap();
        columns[0][field] = if field == "generated" {
            serde_json::json!({"kind":"Stored","expression":expression})
        } else {
            expression.clone()
        };
        let text = columns.to_string();
        assert!(serde_json::from_str::<Vec<ColumnDef>>(&text).is_ok());
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        assert!(read_stored_columns_json(&text, &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}
