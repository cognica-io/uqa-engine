//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod expanded;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    kind: String,
    variables: Vec<Variable>,
}
#[derive(serde::Deserialize)]
struct Variable {
    attribute: usize,
    #[serde(rename = "type")]
    type_oid: u32,
    collation: u32,
}

fn catalog() -> Catalog {
    Catalog(
        ["c", "p", "plain", "n"]
            .into_iter()
            .enumerate()
            .map(|(position, name)| {
                let mut column = ColumnDef::nullable(
                    name,
                    if name == "n" {
                        ColumnType::Integer
                    } else {
                        ColumnType::Text
                    },
                );
                column.collation_oid = [Some(950), Some(951), None, None][position];
                column
            })
            .collect(),
    )
}
fn references(kind: &str) -> Vec<Reference> {
    let cases: Vec<Reference> = serde_json::from_str(include_str!("collations.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        48
    );
    cases.into_iter().filter(|case| case.kind == kind).collect()
}
fn native_query(catalog: &Catalog, case: &Reference, empty: bool, restore: bool) -> QueryPlan {
    let Statement::CreateRule(rule) = crate::compile(&case.sql).unwrap().remove(0) else {
        panic!("rule")
    };
    let columns = catalog.0.iter().map(|column| column.name.clone()).collect();
    let row_type = catalog
        .0
        .iter()
        .map(|column| (column.name.clone(), column.ty.clone()))
        .collect::<Vec<_>>();
    let action =
        expand_rule_action_row_stars(catalog, &rule.actions[0], &columns, &row_type, rule.event)
            .unwrap();
    let predicate = if empty { "FALSE" } else { "TRUE" };
    let sql = match rule.event {
        RuleEvent::Insert if !empty && case.kind == "values" => {
            "INSERT INTO items VALUES ('C value', 'POSIX value', NULL, 7)".into()
        }
        RuleEvent::Insert => {
            format!("INSERT INTO items SELECT 'C value', 'POSIX value', NULL, 7 WHERE {predicate}")
        }
        RuleEvent::Update => format!(
            "UPDATE items SET c='C value', p='POSIX value', plain=NULL, n=7 WHERE {predicate}"
        ),
        RuleEvent::Delete => format!("DELETE FROM items WHERE {predicate}"),
        RuleEvent::Select => unreachable!(),
    };
    let UnifiedPlan::Command(command) = native_action(catalog, &sql, &action, restore) else {
        panic!("INSERT action")
    };
    let CommandPlan::Insert(insert) = *command else {
        panic!("INSERT action")
    };
    *insert.source.expect("native rule action input")
}
fn check_schema(catalog: &Catalog, case: &Reference, query: QueryPlan) {
    let schema =
        crate::binding::analyze_query_plan_schema(catalog, &query, &[], &catalog.binding(), None)
            .unwrap_or_else(|error| panic!("{}: {error}", case.sql));
    for variable in &case.variables {
        let position = variable.attribute - 1;
        assert_eq!(
            schema.column_collation(position),
            Some(variable.collation),
            "{}",
            case.sql
        );
        assert_eq!(
            crate::catalog::type_metadata::pg_type_oid(schema.column_type(position).unwrap()),
            i64::from(variable.type_oid),
            "{}",
            case.sql
        );
    }
}

#[test]
fn rule_values_actions_preserve_postgresql_event_collations() {
    let catalog = catalog();
    for case in references("values") {
        for empty in [false, true] {
            for restore in [false, true] {
                check_schema(
                    &catalog,
                    &case,
                    native_query(&catalog, &case, empty, restore),
                );
            }
        }
    }
}
#[test]
fn rule_set_actions_preserve_postgresql_event_collations_with_empty_inputs() {
    let catalog = catalog();
    for case in references("select") {
        for empty in [false, true] {
            for restore in [false, true] {
                check_schema(
                    &catalog,
                    &case,
                    native_query(&catalog, &case, empty, restore),
                );
            }
        }
    }
}
