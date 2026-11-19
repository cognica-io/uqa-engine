//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{read_stored_table_constraints_json, CancellationToken, MemoryBudget};
use crate::ast::TableConstraintSet;

mod compatibility;
mod retention;

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<TableConstraintSet>(text);
    let memory = MemoryBudget::new(1 << 26);
    let token = CancellationToken::new();
    let actual = read_stored_table_constraints_json(text, &memory, &token, &token);
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

fn fixture() -> serde_json::Value {
    let mut table: serde_json::Value = serde_json::from_str(r#"{
        "dropped_attributes":[{"number":3,"type_length":-1,"type_modifier":22,"dimensions":2,"by_value":false,"alignment":"i","storage":"x","collation":100,"identity":"a","is_local":false,"inheritance_count":2}],
        "columns_declared":true,
        "checks":[{"name":"check_name","object_id":[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"catalog_oid":40001,"is_local":false,"expr":{"Binary":{"op":"Greater","lhs":{"Column":"a"},"rhs":{"Literal":0}}},"enforced":false,"validated":false,"no_inherit":true,"partition_constraint":{"spec":{"strategy":"Range","keys":[{"Column":"a"}]},"bound":{"Range":{"lower":["MinValue"],"upper":["MaxValue"]}}}}],
        "key_constraints":[{"catalog_identity":{"object_id":[2,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"oid":40002},"index_identity":{"object_id":[3,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"oid":40003},"name":"key_name","kind":"Unique","columns":["a","b"],"included_columns":["b","b"],"nulls_not_distinct":true,"without_overlaps":true}],
        "persistence":"Unlogged","on_commit":"PreserveRows",
        "hierarchy":{"parents":["s.one","s.two"],"parent_sequence_numbers":[2,5],"partition_spec":{"strategy":"Range","keys":[{"Column":"a"}]},"partition_bound":{"Range":{"lower":[{"Value":{"Literal":1}},"MinValue"],"upper":["MaxValue",{"Value":{"Literal":9}}]}},"local_columns":["a","b"],"partition_identity_overrides":[{"column":"a","original":{"kind":"serial","sequence":"s.original","owner":{"table":"s.t","column":"a"}}},{"column":"b"}]},
        "catalog_oids":{"relation":40000,"row_type":40004,"array_type":40005,"rule":40006},"row_type_array_name":"_original_array"
    }"#).unwrap();
    let mut foreign = super::complete()[0]["references"].take();
    let fields = foreign.as_object_mut().unwrap();
    let relation = fields.remove("table").unwrap();
    let column = fields.remove("column").unwrap();
    foreign["ref_table"] = relation;
    foreign["ref_columns"] = serde_json::json!([column]);
    foreign["local_columns"] = serde_json::json!(["a"]);
    foreign["on_delete_set_columns"] = serde_json::json!(["a"]);
    table["foreign_keys"] = serde_json::json!([foreign.clone()]);
    table["hierarchy"]["partition_inherited_foreign_keys"] = serde_json::json!([foreign]);
    table["hierarchy"]["partition_inherited_key_constraints"] = table["key_constraints"].clone();
    table
}

#[test]
fn stored_table_constraints_preserve_complete_metadata_and_ordered_families() {
    for text in ["{}", "[]", &fixture().to_string()] {
        assert!(compare(text));
    }
    let mut duplicate = fixture();
    for field in [
        "checks",
        "foreign_keys",
        "key_constraints",
        "dropped_attributes",
    ] {
        let entries = duplicate[field].as_array_mut().unwrap();
        entries.push(entries[0].clone());
    }
    assert!(compare(&duplicate.to_string()));
}

#[test]
fn stored_table_constraints_preserve_partition_variants_and_positional_defaults() {
    for bound in [
        r#""Default""#,
        r#"{"Default":null}"#,
        r#"{"List":[{"Literal":1},{"Literal":null}]}"#,
        r#"{"Range":{"lower":[{"MinValue":null},{"Value":{"Literal":1}}],"upper":["MaxValue",{"Value":{"Literal":9}}]}}"#,
        r#"{"Hash":{"modulus":4,"remainder":2}}"#,
    ] {
        let mut table = fixture();
        table["hierarchy"]["partition_bound"] = serde_json::from_str(bound).unwrap();
        for strategy in ["List", "Range", "Hash"] {
            table["hierarchy"]["partition_spec"]["strategy"] = serde_json::json!(strategy);
            assert!(compare(&table.to_string()));
        }
    }
    for text in [
        r#"{"checks":[[null,null,null,true,{"Literal":true}]]}"#,
        r#"{"key_constraints":[[null,null,null,"PrimaryKey",["a"]]]}"#,
        r#"{"foreign_keys":[[null,null,null,null,null,["a"],"parent",["id"]]]}"#,
        r#"{"catalog_oids":[123],"hierarchy":[]}"#,
        r#"{"hierarchy":{"partition_identity_overrides":[["a"]]}}"#,
    ] {
        assert!(compare(text));
    }
}

#[test]
fn stored_table_constraints_reject_query_and_window_nodes_without_execution() {
    for sql in ["SELECT (SELECT 1)", "SELECT sum(1) OVER ()"] {
        let crate::ast::Statement::Select(select) =
            crate::compiler::compile(sql).unwrap().pop().unwrap()
        else {
            panic!("SELECT fixture");
        };
        let expression = serde_json::to_value(&select.projections[0].expr).unwrap();
        for site in 0..3 {
            let mut table = fixture();
            match site {
                0 => table["checks"][0]["expr"] = expression.clone(),
                1 => {
                    table["hierarchy"]["partition_spec"]["keys"] =
                        serde_json::json!([expression.clone()]);
                }
                _ => {
                    table["hierarchy"]["partition_bound"] =
                        serde_json::json!({"List":[expression.clone()]});
                }
            }
            let text = table.to_string();
            assert!(serde_json::from_str::<TableConstraintSet>(&text).is_ok());
            let memory = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            assert!(read_stored_table_constraints_json(&text, &memory, &token, &token).is_err());
            assert_eq!(memory.used(), 0);
        }
    }
}
