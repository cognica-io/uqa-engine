//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnDef, Expr, Statement, TableCheck};
use std::{cell::RefCell, collections::BTreeSet};
use uqa_core::Value;

#[derive(Default)]
struct Catalog {
    names: BTreeMap<String, String>,
    ids: RefCell<BTreeMap<RelationIdentity, [u8; 16]>>,
    events: RefCell<Vec<String>>,
}
impl StoredSequenceNames for Catalog {
    fn stored_sequence_name(&self, reference: &str) -> Result<String, String> {
        self.events.borrow_mut().push(format!("name:{reference}"));
        self.names
            .get(reference)
            .cloned()
            .ok_or_else(|| format!("unknown stored sequence `{reference}`"))
    }
}
impl SequenceExpressionCatalog for Catalog {
    fn object_ids(&self) -> SequenceExpressionObjectIdsRead<'_> {
        self.events.borrow_mut().push("identities".into());
        Box::new(self.ids.borrow())
    }
}
fn definition(declaration: &str) -> (Vec<ColumnDef>, Vec<TableCheck>) {
    let Statement::CreateTable(table) =
        crate::compile(&format!("CREATE TABLE items({declaration})"))
            .unwrap()
            .remove(0)
    else {
        panic!("table declaration")
    };
    (table.columns, table.checks)
}
fn expression(sql: &str) -> Expr {
    definition(&format!("value bigint DEFAULT ({sql})"))
        .0
        .remove(0)
        .default
        .unwrap()
}
fn catalog() -> Catalog {
    Catalog {
        names: BTreeMap::from([
            ("ids".into(), "public.ids".into()),
            ("other".into(), "tenant.other".into()),
        ]),
        ..Catalog::default()
    }
}
#[test]
fn literal_names_and_typed_regclass_identities_are_unioned_without_rewriting_the_input() {
    let catalog = catalog();
    catalog.ids.borrow_mut().insert(
        RelationIdentity::new("Mixed.Schema", "Quoted.Sequence"),
        [7; 16],
    );
    let oid = crate::catalog::oids::stable_object_oid("relation", &[7; 16]);
    let input = Expr::Array(vec![
        expression("nextval('ids') + currval('ids')"),
        Expr::TypedLiteral {
            value: Value::Int(oid),
            ty: "PG_CATALOG.REGCLASS".into(),
        },
        Expr::Literal(Value::Int(oid)),
        Expr::TypedLiteral {
            value: Value::Int(oid + 1),
            ty: "regclass".into(),
        },
    ]);
    let before = input.clone();
    assert_eq!(
        stored_sequence_targets_in_loaded_expr(&catalog, &input).unwrap(),
        BTreeSet::from([
            "public.ids".into(),
            "\"Mixed.Schema\".\"Quoted.Sequence\"".into()
        ])
    );
    assert_eq!(input, before);
    assert_eq!(
        *catalog.events.borrow(),
        ["name:ids", "name:ids", "identities"]
    );
    assert!(
        catalog.ids.try_borrow_mut().is_ok(),
        "identity snapshot must release its borrowed guard"
    );
}
#[test]
fn missing_literal_identity_stops_before_oid_reads_and_preserves_the_expression() {
    let catalog = catalog();
    let input = expression("nextval('missing')");
    let before = input.clone();
    assert_eq!(
        stored_sequence_targets_in_loaded_expr(&catalog, &input).unwrap_err(),
        "unknown stored sequence `missing`"
    );
    assert_eq!(*catalog.events.borrow(), ["name:missing"]);
    assert_eq!(input, before);
}
#[test]
fn absent_schema_expressions_do_not_acquire_catalog_guards() {
    let catalog = catalog();
    let (columns, checks) = definition("id integer");
    let mut dependents = Vec::new();
    append_sequence_schema_expression_dependents(
        &catalog,
        "public.items",
        &columns,
        &checks,
        "public.ids",
        false,
        &mut dependents,
    )
    .unwrap();
    assert!(dependents.is_empty());
    assert!(catalog.events.borrow().is_empty());
}
#[test]
fn dependency_analysis_preserves_column_and_constraint_order_for_both_relation_kinds() {
    let catalog = catalog();
    let (mut columns,checks)=definition("id bigint DEFAULT nextval('ids') CONSTRAINT inline_check CHECK(nextval('ids') > 0), generated bigint GENERATED ALWAYS AS (1) STORED, CONSTRAINT table_check CHECK(currval('ids') > 0)");
    columns[1].generated.as_mut().unwrap().expression = Box::new(expression("nextval('ids')"));
    let before = serde_json::to_string(&(&columns, &checks)).unwrap();
    for foreign in [false, true] {
        let mut dependents = Vec::new();
        append_sequence_schema_expression_dependents(
            &catalog,
            "public.items",
            &columns,
            &checks,
            "public.ids",
            foreign,
            &mut dependents,
        )
        .unwrap();
        assert_eq!(
            dependents,
            vec![
                SequenceSchemaDependent::Default {
                    table: "public.items".into(),
                    column: "id".into(),
                    foreign
                },
                SequenceSchemaDependent::CheckConstraint {
                    table: "public.items".into(),
                    constraint: "inline_check".into(),
                    foreign
                },
                SequenceSchemaDependent::GeneratedColumn {
                    table: "public.items".into(),
                    column: "generated".into(),
                    foreign
                },
                SequenceSchemaDependent::CheckConstraint {
                    table: "public.items".into(),
                    constraint: "table_check".into(),
                    foreign
                }
            ]
        );
        assert_eq!(serde_json::to_string(&(&columns, &checks)).unwrap(), before);
    }
}
#[test]
fn unnamed_checks_fail_only_when_their_expression_references_the_requested_sequence() {
    let catalog = catalog();
    let (mut columns, mut checks) =
        definition("id bigint CHECK(nextval('ids') > 0), CHECK(currval('ids') > 0)");
    columns[0].check_name = None;
    checks[0].name = None;
    for foreign in [false, true] {
        let relation = if foreign {
            "foreign table `public.items`"
        } else {
            "`public.items`"
        };
        let mut dependents = Vec::new();
        append_sequence_schema_expression_dependents(
            &catalog,
            "public.items",
            &columns,
            &checks,
            "tenant.other",
            foreign,
            &mut dependents,
        )
        .unwrap();
        assert!(dependents.is_empty());
        assert_eq!(
            append_sequence_schema_expression_dependents(
                &catalog,
                "public.items",
                &columns,
                &[],
                "public.ids",
                foreign,
                &mut dependents
            )
            .unwrap_err(),
            format!("CHECK constraint on {relation}.`id` has no catalog name")
        );
        assert_eq!(
            append_sequence_schema_expression_dependents(
                &catalog,
                "public.items",
                &[],
                &checks,
                "public.ids",
                foreign,
                &mut dependents
            )
            .unwrap_err(),
            format!("table CHECK constraint on {relation} has no catalog name")
        );
    }
}
