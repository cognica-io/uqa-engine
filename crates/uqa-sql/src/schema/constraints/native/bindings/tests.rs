//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Statement,
    binding::fixture::{empty_binding_context, NoRoutines},
    catalog::{
        relation_oids::RelationCatalogOids,
        type_definitions::TypeDefinitions,
        type_references::{TypeReferenceInput, TypeReferences},
    },
    routines::native::NativeRoutineReferences,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn validate(sql: &str, memory: &MemoryBudget, token: &CancellationToken) -> Result<(), SQLError> {
    let statement = crate::compile(sql).unwrap().remove(0);
    let original = serde_json::to_value(&statement).unwrap();
    let control = ProductionControl::new(memory, token, token);
    let binding = empty_binding_context().copy_with_control(&control)?;
    let references = NativeRoutineReferences::new(std::iter::empty(), memory, token)?;
    let input = TypeReferenceInput {
        domains: &TypeDefinitions::default(),
        enums: &TypeDefinitions::default(),
        composites: &TypeDefinitions::default(),
        ranges: &TypeDefinitions::default(),
        shells: &TypeDefinitions::default(),
    };
    let types = TypeReferences::new(input, std::iter::empty(), memory, token)?;
    let context = NativeScalarContext {
        subject: "constraint",
        routines: &NoRoutines,
        references: &references,
        types: &types,
        binding: &binding,
    };
    let result = match &statement {
        Statement::CreateTable(table) => {
            let name = RelationIdentity::from_legacy_name(&table.name).unwrap();
            let row_type = RelationCatalogOids {
                relation: 80001,
                row_type: Some(80002),
                array_type: Some(80003),
                rule: None,
            }
            .row_type_reference(&name)
            .map(ColumnType::Composite)
            .unwrap();
            validate_table(
                &name,
                &table.columns,
                &table.checks,
                Some(&row_type),
                &context,
                &control,
            )
        }
        Statement::CreateDomain(domain) => validate_domain(domain, &context, &control),
        _ => panic!("CHECK declaration"),
    };
    assert_eq!(serde_json::to_value(&statement).unwrap(), original);
    result
}

#[test]
fn stored_constraint_bindings_preserve_boolean_rows_and_unevaluated_expressions() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "CREATE TABLE items(id integer CHECK(id > 0), flag boolean CHECK(flag))",
        "CREATE TABLE items(id integer, CHECK(items.id > 0 AND tableoid > 0::oid))",
        "CREATE TABLE items(id integer, CHECK((items).id > 0 AND items.tableoid > 0::oid))",
        "CREATE TABLE items(id integer, CHECK(1 / 0 = 0))",
        "CREATE TABLE items(id integer, CHECK(CASE WHEN id > 0 THEN true ELSE NULL END))",
        "CREATE TABLE items(id integer, CHECK(NULL), CHECK('true'))",
        "CREATE TABLE items(id integer, CHECK('true' COLLATE \"C\"))",
        "CREATE DOMAIN positive AS integer CHECK(VALUE > 0)",
        "CREATE DOMAIN truth AS boolean CHECK(VALUE)",
        "CREATE DOMAIN latent AS integer CHECK(1 / 0 = VALUE)",
        "CREATE DOMAIN unknown_condition AS integer CHECK(NULL)",
        "CREATE DOMAIN collated_condition AS integer CHECK('true' COLLATE \"C\")",
    ] {
        validate(sql, &memory, &token).unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(memory.used(), 0, "{sql}");
    }
}

#[test]
fn stored_constraint_bindings_reject_wrong_types_and_non_scalar_inputs() {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "CREATE TABLE items(id integer CHECK(id))",
        "CREATE TABLE items(id integer, CHECK(id))",
        "CREATE TABLE items(id integer, CHECK(true AND id))",
        "CREATE TABLE items(id integer, CHECK(other.id > 0))",
        "CREATE TABLE items(id integer, CHECK(missing > 0))",
        "CREATE TABLE items(id integer, CHECK('invalid_boolean'))",
        "CREATE TABLE items(id integer, CHECK('invalid_boolean' COLLATE \"C\"))",
        "CREATE TABLE items(id integer, CHECK($1))",
        "CREATE TABLE items(id integer, CHECK(sum(id) > 0))",
        "CREATE TABLE items(id integer, CHECK(unnest(ARRAY[id]) > 0))",
        "CREATE TABLE items(id integer, CHECK(row_number() OVER () > 0))",
        "CREATE TABLE items(id integer, CHECK((SELECT true)))",
        "CREATE DOMAIN positive AS integer CHECK(VALUE)",
        "CREATE DOMAIN positive AS integer CHECK(tableoid > 0)",
        "CREATE DOMAIN positive AS integer CHECK(other.value > 0)",
        "CREATE DOMAIN collated_condition AS integer CHECK('invalid_boolean' COLLATE \"C\")",
    ] {
        assert!(validate(sql, &memory, &token).is_err(), "{sql}");
        assert_eq!(memory.used(), 0, "{sql}");
    }
}

#[test]
fn stored_constraint_bindings_release_denied_and_cancelled_analysis() {
    let sql = "CREATE TABLE items(id integer, CHECK((items).id > 0))";
    let mut successes = 0;
    let mut denials = 0;
    for limit in [0, 32, 512, 4096, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        match validate(sql, &memory, &CancellationToken::new()) {
            Ok(()) => successes += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"), "{error}");
                denials += 1;
            }
        }
        assert_eq!(memory.used(), 0);
    }
    assert!(successes > 0 && denials > 0);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    token.cancel();
    assert!(validate(sql, &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
