//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::Statement;
use crate::catalog::events::definition::tests::fixtures::Catalog;

#[derive(serde::Deserialize)]
struct Reference {
    sql: String,
    variables: Vec<Variable>,
}
#[derive(serde::Deserialize)]
struct Variable {
    relation: u32,
    attribute: usize,
    #[serde(rename = "type")]
    type_oid: u32,
    collation: u32,
}

#[test]
fn trigger_condition_input_collations_match_postgresql_event_variables() {
    let columns: Vec<_> = ["c", "p", "plain", "n"]
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
        .collect();
    let catalog = Catalog {
        columns: Some(columns.clone()),
        ..Catalog::default()
    };
    let foreign = Catalog {
        kind: "foreign table",
        columns: Some(columns.clone()),
        ..Catalog::default()
    };
    for catalog in [&catalog, &foreign] {
        let observed = catalog
            .context()
            .trigger_relation_definitions("public.items")
            .unwrap();
        for (actual, expected) in observed.iter().zip(&columns) {
            assert_eq!(actual.name, expected.name);
            assert_eq!(actual.ty, expected.ty);
            assert_eq!(actual.collation_oid, expected.collation_oid);
        }
        assert_eq!(observed.len(), columns.len());
    }
    let cases: Vec<Reference> =
        serde_json::from_str(include_str!("tests/collations.json")).unwrap();
    assert_eq!(
        cases.iter().map(|case| case.variables.len()).sum::<usize>(),
        16
    );
    for case in cases {
        let Statement::CreateTrigger(trigger) = crate::compile(&case.sql).unwrap().remove(0) else {
            panic!("trigger")
        };
        let mut condition = trigger.when.clone().unwrap();
        catalog
            .context()
            .validate_trigger_condition(
                &trigger,
                &columns,
                &mut condition,
                RelationLookupMode::Dynamic,
            )
            .unwrap();
        for variable in case.variables {
            let side = match variable.relation {
                1 => "old",
                2 => "new",
                _ => panic!("event relation"),
            };
            let expression = Expr::qualified_column(side, &columns[variable.attribute - 1].name);
            let Expr::TypedLiteral {
                ty, collation_oid, ..
            } = bind_expr(
                &expression,
                &mut TriggerConditionTypeResolver { columns: &columns },
            )
            .unwrap()
            else {
                panic!("typed event field")
            };
            let ty = ColumnType::from_sql_name(&ty).unwrap();
            assert_eq!(
                crate::catalog::type_metadata::pg_type_oid(&ty),
                i64::from(variable.type_oid)
            );
            assert_eq!(
                collation_oid
                    .map(i64::from)
                    .unwrap_or_else(|| crate::catalog::type_metadata::pg_type_collation_oid(&ty)),
                i64::from(variable.collation),
                "{}",
                case.sql
            );
        }
    }
}

#[test]
fn trigger_conditions_reject_set_returning_calls_before_row_checks() {
    let columns = vec![
        ColumnDef::nullable("s", ColumnType::Text),
        ColumnDef::nullable("n", ColumnType::Integer),
    ];
    let catalog = Catalog {
        columns: Some(columns.clone()),
        ..Catalog::default()
    };
    let context = catalog.context();
    let condition = |sql: &str| {
        let Statement::CreateTrigger(trigger) = crate::compile(sql).unwrap().remove(0) else {
            panic!("trigger")
        };
        let condition = trigger.when.clone().unwrap();
        (trigger, condition)
    };
    // An INSERT trigger cannot read OLD, but the set-returning call is rejected first, while the condition is analyzed. A stored condition keeps loading, and only the checks made after analysis still apply to it.
    for (sql, stored_error) in [
        (
            "CREATE TRIGGER t AFTER INSERT ON items FOR EACH ROW WHEN (regexp_matches(OLD.s, 'a') IS NOT NULL) EXECUTE FUNCTION f()",
            Some("INSERT trigger's WHEN condition cannot reference OLD values"),
        ),
        (
            "CREATE TRIGGER t AFTER INSERT ON items FOR EACH STATEMENT WHEN (generate_series(1, 2) > NEW.n) EXECUTE FUNCTION f()",
            Some("statement trigger's WHEN condition cannot reference column values"),
        ),
        (
            "CREATE TRIGGER t BEFORE INSERT ON items FOR EACH ROW WHEN (NOT generate_series(0, 1)::boolean) EXECUTE FUNCTION f()",
            None,
        ),
    ] {
        let (trigger, mut when) = condition(sql);
        let error = context
            .validate_trigger_condition(&trigger, &columns, &mut when, RelationLookupMode::Dynamic)
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}");
        assert_eq!(
            error.to_string(),
            "set-returning functions are not allowed in trigger WHEN conditions",
            "{sql}"
        );
        let (trigger, mut when) = condition(sql);
        let stored = context.validate_trigger_condition(
            &trigger,
            &columns,
            &mut when,
            RelationLookupMode::Bound,
        );
        assert_eq!(
            stored.err().map(|error| error.to_string()).as_deref(),
            stored_error,
            "{sql}"
        );
    }
}
