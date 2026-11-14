//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;
use crate::binding::fixture::{empty_binding_context, NoRoutines};
use crate::catalog::type_definitions::TypeDefinitions;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn declaration(sql: &str) -> crate::ast::CreateIndex {
    let Statement::CreateIndex(index) = crate::compile(sql).unwrap().remove(0) else {
        panic!("index declaration");
    };
    index
}

fn check(
    index: &crate::ast::CreateIndex,
    types: Vec<ColumnType>,
    declared: bool,
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(memory, token, token);
    let binding = empty_binding_context().copy_with_control(&control)?;
    let references = NativeRoutineReferences::new(std::iter::empty(), memory, token)?;
    let type_input = crate::catalog::type_references::TypeReferenceInput {
        domains: &TypeDefinitions::default(),
        enums: &TypeDefinitions::default(),
        composites: &TypeDefinitions::default(),
        ranges: &TypeDefinitions::default(),
        shells: &TypeDefinitions::default(),
    };
    let types_catalog = crate::catalog::type_references::TypeReferences::new(
        type_input,
        std::iter::empty(),
        memory,
        token,
    )?;
    let columns = [
        ColumnDef::nullable("id", ColumnType::Integer),
        ColumnDef::nullable("label", ColumnType::Text),
        ColumnDef::nullable(
            "flag",
            ColumnType::Domain {
                schema: "public".into(),
                name: "truth".into(),
                oid: 50001,
                array_oid: Some(50002),
                collation_oid: None,
                base: Box::new(ColumnType::Boolean),
            },
        ),
    ];
    let relation = crate::RelationIdentity::new("public", "items");
    let row_type = crate::catalog::relation_oids::RelationCatalogOids {
        relation: 80001,
        row_type: Some(80002),
        array_type: Some(80003),
        rule: None,
    }
    .row_type_reference(&relation)
    .map(ColumnType::Composite)
    .unwrap();
    validate_bindings(
        &index.columns,
        &IndexDefinition {
            key_types: types,
            predicate: index.predicate.clone(),
            ..IndexDefinition::default()
        },
        declared.then_some(columns.as_slice()),
        &IndexBindingContext {
            relation: Some(&relation),
            row_type: Some(&row_type),
            routines: &NoRoutines,
            references: &references,
            types: &types_catalog,
            binding: &binding,
        },
        &control,
    )
}

#[test]
fn stored_index_bindings_preserve_original_expressions_without_evaluation() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, types) in [
        ("CREATE INDEX saved ON items (id, lower(label)) WHERE id > 0", vec![ColumnType::Integer, ColumnType::Text]),
        ("CREATE INDEX saved ON items ((id / 0)) WHERE NULL", vec![]),
        ("CREATE INDEX saved ON items ((CASE WHEN id > 0 THEN label ELSE 'fallback' END)) WHERE label IS NOT NULL", vec![ColumnType::Text]),
        ("CREATE INDEX saved ON items ((ARRAY[id, id])) WHERE id IS NOT NULL", vec![ColumnType::Array(Box::new(ColumnType::Integer))]),
        ("CREATE INDEX saved ON items (((items).id)) WHERE flag", vec![ColumnType::Integer]),
        ("CREATE INDEX saved ON items (((items).label)) WHERE (items).flag", vec![ColumnType::Text]),
    ] {
        let index = declaration(sql);
        let original = serde_json::to_value(&index).unwrap();
        check(&index, types, true, &memory, &token).unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(serde_json::to_value(&index).unwrap(), original);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_index_bindings_reject_invalid_rows_predicates_and_non_scalar_inputs() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "CREATE INDEX saved ON items ((missing + 1))",
        "CREATE INDEX saved ON items ((lower(missing)))",
        "CREATE INDEX saved ON items (id) WHERE missing > 0",
        "CREATE INDEX saved ON items (id) WHERE id",
        "CREATE INDEX saved ON items (id) WHERE true AND id",
        "CREATE INDEX saved ON items (id) WHERE 'invalid_boolean'",
        "CREATE INDEX saved ON items ((CASE WHEN id THEN label ELSE 'x' END))",
        "CREATE INDEX saved ON items ((unknown_routine(id)))",
        "CREATE INDEX saved ON items (($1 + id))",
        "CREATE INDEX saved ON items ((sum(id)))",
        "CREATE INDEX saved ON items ((unnest(ARRAY[id])))",
        "CREATE INDEX saved ON items ((row_number() OVER ()))",
    ] {
        let index = declaration(sql);
        assert!(
            check(&index, vec![], true, &memory, &token).is_err(),
            "{sql}"
        );
        assert_eq!(memory.used(), 0, "{sql}");
    }
}

#[test]
fn stored_index_bindings_compare_recorded_result_types_and_keep_legacy_absence() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let index = declaration("CREATE INDEX saved ON items (id, lower(label))");
    for types in [
        vec![ColumnType::Integer],
        vec![ColumnType::Text, ColumnType::Text],
        vec![ColumnType::Integer, ColumnType::Integer],
    ] {
        assert!(check(&index, types, true, &memory, &token).is_err());
        assert_eq!(memory.used(), 0);
    }
    check(&index, vec![], true, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_bindings_preserve_open_document_fields_and_release_rejected_owners() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let index = declaration("CREATE INDEX saved ON items ((lower(label))) WHERE field IS NOT NULL");
    check(&index, vec![], false, &memory, &token).unwrap();
    assert_eq!(memory.used(), 0);
    let denied = MemoryBudget::new(0);
    assert!(check(&index, vec![], false, &denied, &token).is_err());
    assert_eq!(denied.used(), 0);
    token.cancel();
    assert!(check(&index, vec![], false, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}

#[test]
fn stored_index_key_decoder_preserves_scalar_payloads_and_rejects_query_keys() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let keys =
        declaration("CREATE INDEX saved ON items (id, lower(label), (ARRAY[id, 2]))").columns;
    let json = serde_json::to_string(&keys).unwrap();
    let decoded = IndexKey::read_stored(&json, &memory, &token, &token).unwrap();
    assert_eq!(*decoded, keys);
    assert!(memory.used() > 0);
    drop(decoded);
    assert_eq!(memory.used(), 0);
    assert!(IndexKey::read_stored(r#"[{"ScalarSubquery":{}}]"#, &memory, &token, &token).is_err());
    assert_eq!(memory.used(), 0);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(IndexKey::read_stored(&json, &memory, &token, &cancelled).is_err());
    assert_eq!(memory.used(), 0);
    assert!(IndexKey::read_stored(&json, &MemoryBudget::new(0), &token, &token).is_err());
}
