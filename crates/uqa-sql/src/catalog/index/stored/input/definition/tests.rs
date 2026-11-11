//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnType, Expr};
use uqa_core::{catalog_identity::CatalogObjectIdentity, Value};

mod compatibility;
mod retention;

fn fixture() -> IndexDefinition {
    IndexDefinition {
        catalog: Some(IndexCatalogIdentity {
            identity: CatalogObjectIdentity {
                object_id: [1; 16],
                oid: 40001,
            },
            table_object_id: [2; 16],
            physical_key: "physical.\"key".into(),
        }),
        relationships: IndexRelationships {
            owning_constraint: Some([3; 16]),
            parent_index: Some([4; 16]),
        },
        included_columns: vec!["other".into(), "other".into()],
        column_order: vec![
            IndexColumnOrder {
                descending: false,
                nulls_first: true,
            },
            IndexColumnOrder {
                descending: true,
                nulls_first: false,
            },
        ],
        key_names: vec!["key".into(), "expression".into()],
        key_types: vec![
            ColumnType::Array(Box::new(ColumnType::Named("schema.type".into()))),
            ColumnType::Integer,
        ],
        predicate: Some(Box::new(Expr::Not(Box::new(Expr::Literal(Value::Bool(
            false,
        )))))),
        unique: true,
        nulls_not_distinct: true,
    }
}

fn compare(text: &str) -> bool {
    let expected = serde_json::from_str::<IndexDefinition>(text);
    let memory = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let actual = definition(Some(text), &memory, &token, &token);
    let accepted = expected.is_ok();
    assert_eq!(actual.is_ok(), accepted, "{text}");
    if let (Ok(expected), Ok(actual)) = (expected, actual) {
        assert_eq!(
            serde_json::to_value(&*actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{text}"
        );
    }
    assert_eq!(memory.used(), 0, "{text}");
    accepted
}

#[test]
fn stored_index_definition_preserves_complete_fields_and_legacy_absence() {
    assert!(compare(&serde_json::to_string(&fixture()).unwrap()));
    assert!(compare(r#"{"unique":false,"nulls_not_distinct":false}"#));
    let memory = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let absent = definition(None, &memory, &token, &token).unwrap();
    assert_eq!(*absent, IndexDefinition::default());
    assert_eq!(memory.peak(), 0);
}

#[test]
fn stored_index_definition_preserves_positional_forms_and_column_order() {
    for text in [
        r"[null,[],[],[],[],[],null,false,true]",
        r#"{"column_order":[[true,false],[false,true]],"relationships":[null],"unique":false,"nulls_not_distinct":true}"#,
        r#"{"catalog":[[[1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],40001],[2,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"physical"],"unique":true,"nulls_not_distinct":false}"#,
    ] {
        assert!(compare(text));
    }
    let mut native = fixture();
    for sql in [
        "SELECT abs(-7)",
        "SELECT CASE WHEN true THEN 1 ELSE 2 END",
        "SELECT ARRAY[1,2]",
    ] {
        let crate::ast::Statement::Select(mut select) =
            crate::compiler::compile(sql).unwrap().pop().unwrap()
        else {
            panic!("SELECT fixture")
        };
        native.predicate = Some(Box::new(select.projections.remove(0).expr));
        assert!(compare(&serde_json::to_string(&native).unwrap()));
    }
}

#[test]
fn stored_index_definition_rejects_query_and_window_predicates_without_execution() {
    for sql in ["SELECT (SELECT 1)", "SELECT sum(1) OVER ()"] {
        let crate::ast::Statement::Select(mut select) =
            crate::compiler::compile(sql).unwrap().pop().unwrap()
        else {
            panic!("SELECT fixture")
        };
        let mut native = fixture();
        native.predicate = Some(Box::new(select.projections.remove(0).expr));
        let text = serde_json::to_string(&native).unwrap();
        assert!(serde_json::from_str::<IndexDefinition>(&text).is_ok());
        let memory = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        assert!(definition(Some(&text), &memory, &token, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
}
