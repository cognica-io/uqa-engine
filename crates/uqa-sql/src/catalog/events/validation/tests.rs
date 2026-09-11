//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{plpgsql::bind_expr, RowSchema};

fn trigger() -> CreateTrigger {
    let Statement::CreateTrigger(definition) = crate::compile(
        "CREATE TRIGGER inspected BEFORE INSERT ON items FOR EACH ROW EXECUTE FUNCTION handler()",
    )
    .unwrap()
    .remove(0) else {
        panic!("expected trigger definition")
    };
    definition
}
fn expression(sql: &str) -> Expr {
    let Statement::Select(select) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected SELECT")
    };
    select.projections[0].expr.clone()
}
fn routine_error(error: SQLError, expected_state: &str, expected_message: &str) {
    let SQLError::Routine { sqlstate, message } = error else {
        panic!("expected SQL routine error")
    };
    assert_eq!(sqlstate, expected_state);
    assert_eq!(message, expected_message);
}

#[test]
fn unqualified_rule_conditions_bind_to_the_available_event_row() {
    let columns = vec![("id".into(), ColumnType::Integer)];
    let input = Expr::Column("id".into());
    let inserted = bind_expr(
        &input,
        &mut RuleConditionNameResolver {
            columns: &columns,
            event: RuleEvent::Insert,
        },
    )
    .unwrap();
    let deleted = bind_expr(
        &input,
        &mut RuleConditionNameResolver {
            columns: &columns,
            event: RuleEvent::Delete,
        },
    )
    .unwrap();
    assert_eq!(inserted, Expr::qualified_column("new", "id"));
    assert_eq!(deleted, Expr::qualified_column("old", "id"));
    assert!(
        matches!(bind_expr(&input, &mut RuleConditionNameResolver { columns: &columns, event: RuleEvent::Update }), Err(SQLError::AmbiguousColumn(name)) if name == "id")
    );
}

#[test]
fn unavailable_rule_rows_are_rejected_before_column_lookup() {
    let mut resolver = RuleRowTypeResolver {
        columns: &[],
        event: RuleEvent::Insert,
    };
    routine_error(
        resolver.resolve_qualified("OLD", "missing").unwrap_err(),
        "42P17",
        "there is no OLD relation for INSERT rule",
    );
    assert!(
        matches!(resolver.resolve_qualified("NEW", "missing"), Err(SQLError::UnknownColumn(name)) if name == "NEW.missing")
    );
    let columns = vec![("id".into(), ColumnType::Integer)];
    let typed_row = RuleRowTypeResolver {
        columns: &columns,
        event: RuleEvent::Update,
    }
    .resolve_qualified("OLD", "id")
    .unwrap()
    .unwrap();
    assert_eq!(typed_row.value, Value::Null);
    assert_eq!(typed_row.declared_type.as_deref(), Some("integer"));
}

#[test]
fn returning_checks_width_before_types_and_preserves_unknown_literal_slots() {
    let columns = vec![
        ("id".into(), ColumnType::Integer),
        ("label".into(), ColumnType::Varchar(Some(8))),
    ];
    let short = RowSchema::with_types(vec!["label".into()], vec![Some(ColumnType::Boolean)]);
    routine_error(
        validate_rule_returning_shape(&short, &columns).unwrap_err(),
        "42P17",
        "RETURNING list has too few entries",
    );
    let schema = RowSchema::with_types(
        vec!["id".into(), "label".into()],
        vec![None, Some(ColumnType::Varchar(Some(4)))],
    );
    let error = validate_rule_returning_shape(&schema, &columns).unwrap_err();
    assert!(
        matches!(error, SQLError::Routine { sqlstate, message } if sqlstate == "42P17" && message.starts_with("RETURNING list's entry 2 has different size from column \"label\""))
    );
    let unknown = RowSchema::with_types(vec!["id".into(), "label".into()], vec![None, None]);
    validate_rule_returning_shape(&unknown, &columns).unwrap();
}

#[test]
fn trigger_subquery_restrictions_precede_unqualified_or_unavailable_row_errors() {
    let definition = trigger();
    let condition = expression("SELECT missing AND EXISTS (SELECT OLD.missing)");
    routine_error(
        validate_trigger_condition_references(&definition, &[], &condition).unwrap_err(),
        "0A000",
        "cannot use subquery in trigger WHEN condition",
    );
    let condition = expression("SELECT missing AND OLD.missing");
    routine_error(
        validate_trigger_condition_references(&definition, &[], &condition).unwrap_err(),
        "42P01",
        "trigger WHEN condition must qualify row columns with OLD or NEW",
    );
    routine_error(
        validate_trigger_condition_references(
            &definition,
            &[],
            &Expr::qualified_column("old", "missing"),
        )
        .unwrap_err(),
        "42P17",
        "INSERT trigger's WHEN condition cannot reference OLD values",
    );
}

#[test]
fn transition_metadata_errors_preserve_row_variable_hierarchy_and_timing_order() {
    let mut definition = trigger();
    let mut hierarchy = TableHierarchy::default();
    hierarchy.parents.push("public.parent".into());
    let mut transition = TriggerTransitionRelation {
        name: "rows".into(),
        is_new: true,
        is_table: false,
    };
    routine_error(
        validate_trigger_transition_relation(&definition, &hierarchy, &transition).unwrap_err(),
        "0A000",
        "ROW variable naming in the REFERENCING clause is not supported",
    );
    transition.is_table = true;
    routine_error(
        validate_trigger_transition_relation(&definition, &hierarchy, &transition).unwrap_err(),
        "0A000",
        "ROW triggers with transition tables are not supported on inheritance children",
    );
    definition.row = false;
    routine_error(
        validate_trigger_transition_relation(&definition, &hierarchy, &transition).unwrap_err(),
        "42P17",
        "transition table name can only be specified for an AFTER trigger",
    );
}

struct UnusedCatalog;
impl RuleSourceCatalog for UnusedCatalog {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        panic!("invalid namespaces must fail before relation metadata reads")
    }
    fn rule_relation_columns(&self, _: &str) -> Result<Vec<(String, ColumnType)>, SQLError> {
        panic!("invalid namespaces must fail before target metadata reads")
    }
}

#[test]
fn nested_rule_cte_and_alias_restrictions_do_not_capture_relation_metadata() {
    let statement = crate::compile("WITH q AS (SELECT NEW.id) SELECT 1")
        .unwrap()
        .remove(0);
    routine_error(
        validate_rule_action_reference_scopes(&UnusedCatalog, &statement).unwrap_err(),
        "0A000",
        "cannot refer to NEW within WITH query",
    );
    let statement = crate::compile("SELECT 1 FROM items AS OLD")
        .unwrap()
        .remove(0);
    routine_error(
        validate_rule_action_reference_scopes(&UnusedCatalog, &statement).unwrap_err(),
        "42712",
        "table name \"old\" specified more than once",
    );
}
