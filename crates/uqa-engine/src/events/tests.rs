//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_sql::ast::RuleEvent;
use uqa_sql::ir::ScalarExpr;
#[test]
fn stored_rule_conditions_use_only_structural_event_row_references() {
    let engine = crate::Engine::new();
    engine
        .sql(
            "CREATE TABLE structural_rule_items(id integer);
                 CREATE RULE structural_rule_condition AS ON INSERT TO structural_rule_items
                   WHERE EXISTS (SELECT 1 WHERE NEW.id > 0) DO NOTHING",
            &[],
        )
        .unwrap();
    let rule = engine
        .rules_for("public.structural_rule_items", RuleEvent::Insert)
        .unwrap()
        .pop()
        .unwrap();
    let (plan, binding) = rule.bound_condition_plan().unwrap();
    let expected = binding.new_column("id").unwrap();
    let mut internal_columns = Vec::new();
    let mut contains_nul_qualifier = false;
    let mut inspect = |expression: &mut ScalarExpr| match expression {
        ScalarExpr::InternalColumn(column) => internal_columns.push(*column),
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            contains_nul_qualifier |= qualifier.contains('\0');
        }
        _ => {}
    };
    let mut scalar = plan.scalar.clone();
    uqa_planner::rewrite_scalar_expression(&mut scalar, &mut inspect);
    for subquery in &plan.subqueries {
        let mut subquery = subquery.clone();
        subquery.rewrite_scalar_expressions(&mut inspect);
    }
    assert!(internal_columns.contains(&expected));
    assert!(!contains_nul_qualifier);

    let serialized = serde_json::to_string(&rule).unwrap();
    assert!(!serialized.contains("\\u0000"));
    assert!(!serialized.contains('\0'));
}

#[test]
fn renamed_deferred_triggers_fire_and_dropped_triggers_forget_pending_events() {
    let engine = crate::Engine::new();
    engine.sql("CREATE TABLE event_items(id integer); CREATE TABLE event_audit(id integer); CREATE FUNCTION event_handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO event_audit VALUES (NEW.id); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER event_check AFTER INSERT ON event_items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION event_handler()",&[]).unwrap();
    engine.sql("BEGIN; INSERT INTO event_items VALUES (1); ALTER TRIGGER event_check ON event_items RENAME TO renamed_event_check; COMMIT",&[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM event_audit ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![std::collections::BTreeMap::from([(
            "id".into(),
            uqa_core::Value::Int(1)
        )])]
    );
    engine.sql("BEGIN; INSERT INTO event_items VALUES (2); DROP TRIGGER renamed_event_check ON event_items; COMMIT",&[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT id FROM event_audit ORDER BY id", &[])
            .unwrap()
            .rows,
        vec![std::collections::BTreeMap::from([(
            "id".into(),
            uqa_core::Value::Int(1)
        )])]
    );
    assert!(engine.durable.triggers.read().is_empty());
}
#[test]
fn failed_constraint_trigger_replacement_preserves_registered_identity_and_definition() {
    let engine = crate::Engine::new();
    engine.sql("CREATE TABLE event_items(id integer); CREATE FUNCTION event_handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER event_check AFTER INSERT ON event_items DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION event_handler()",&[]).unwrap();
    let before = serde_json::to_string(&engine.list_triggers()).unwrap();
    let error=engine.sql("CREATE OR REPLACE TRIGGER event_check BEFORE UPDATE ON event_items FOR EACH ROW EXECUTE FUNCTION event_handler()",&[]).unwrap_err();
    assert!(
        matches!(error,uqa_sql::SQLError::Routine {sqlstate,message} if sqlstate=="0A000" && message=="CREATE OR REPLACE CONSTRAINT TRIGGER is not supported")
    );
    assert_eq!(
        serde_json::to_string(&engine.list_triggers()).unwrap(),
        before
    );
}
