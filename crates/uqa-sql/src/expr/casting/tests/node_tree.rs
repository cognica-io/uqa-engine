//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input rejection differs from copying a catalog-owned node tree.

use super::*;
use crate::ColumnType;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn external_node_tree_input_matches_postgresql_and_preserves_typed_values() {
    let text = Value::Str("{CONST :consttype 23}".into());
    for source in [
        None,
        Some("text"),
        Some("varchar"),
        Some("bpchar"),
        Some("name"),
    ] {
        let error = cast_value_from(&text, "pg_node_tree", source).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"));
        assert_eq!(
            error.to_string(),
            "cannot accept a value of type pg_node_tree"
        );
    }
    for (value, source, name) in [
        (Value::Int(1), "integer", "integer"),
        (Value::Bool(true), "boolean", "boolean"),
        (Value::Str("x".into()), "\"char\"", "\"char\""),
    ] {
        let error = cast_value_from(&value, "pg_node_tree", Some(source)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42846"));
        assert_eq!(
            error.to_string(),
            format!("cannot cast type {name} to pg_node_tree")
        );
    }
    assert_eq!(
        cast_value(&Value::Null, "pg_node_tree").unwrap(),
        Value::Null
    );
    assert_eq!(
        cast_value_from(&text, "text", Some("pg_node_tree")).unwrap(),
        text
    );
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let copy = cast_value_from_with_control(&text, "pg_node_tree", Some("pg_node_tree"), &control)
        .unwrap();
    assert_eq!(*copy, text);
    assert!(copy.reserved_bytes() > 0);
    assert_eq!(budget.used(), copy.reserved_bytes());
    drop(copy);
    assert_eq!(budget.used(), 0);
}

#[test]
fn node_tree_arrays_do_not_exist_even_for_null_input() {
    for name in [
        "pg_node_tree[]",
        "pg_catalog.pg_node_tree[]",
        "PG_CATALOG.pg_node_tree[]",
    ] {
        assert_eq!(
            ColumnType::from_sql_name(name).unwrap_err().sqlstate(),
            Some("42704")
        );
        assert_eq!(
            cast_value(&Value::Null, name).unwrap_err().sqlstate(),
            Some("42704")
        );
    }
    assert_eq!(
        cast_value(&Value::Null, "PG_CATALOG.void[]")
            .unwrap_err()
            .sqlstate(),
        Some("42704")
    );
    assert!(ColumnType::from_sql_name("_pg_node_tree").is_err());
    let error = crate::compile("CREATE TABLE t(n pg_node_tree[])").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42704"));
    assert_eq!(error.to_string(), "type \"pg_node_tree[]\" does not exist");
}
