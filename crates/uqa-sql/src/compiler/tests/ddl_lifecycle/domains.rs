//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AlterDomainAction, FunctionDispatch};
use crate::plan::{CommandPlan, UnifiedPlan};
use uqa_core::Value;

#[test]
fn domain_constraint_commands_preserve_action_and_validation_state() {
    let statements = crate::compile(
        r#"ALTER DOMAIN "Schema"."Domain.Name" ADD CONSTRAINT "Positive" CHECK (VALUE > 0) NOT VALID;
           ALTER DOMAIN public.amount ADD CHECK (VALUE < 100) NO INHERIT;
           ALTER DOMAIN public.amount ADD CONSTRAINT required NOT NULL;
           ALTER DOMAIN public.amount DROP CONSTRAINT IF EXISTS positive CASCADE;
           ALTER DOMAIN public.amount VALIDATE CONSTRAINT positive"#,
    )
    .unwrap();
    for statement in &statements {
        let UnifiedPlan::Command(plan) = UnifiedPlan::lower(statement.clone()) else {
            panic!("ALTER DOMAIN command");
        };
        assert_eq!(plan.name(), "AlterDomain");
        assert_eq!(
            crate::result::completion::command_tag_name(&plan),
            "ALTER DOMAIN"
        );
        assert!(matches!(plan.as_ref(), CommandPlan::AlterDomain(_)));
    }
    let actions = statements
        .into_iter()
        .map(|statement| {
            let Statement::AlterDomain(alter) = statement else {
                panic!("ALTER DOMAIN statement");
            };
            alter
        })
        .collect::<Vec<_>>();
    assert_eq!(actions[0].name, "\"Schema\".\"Domain.Name\"");
    let AlterDomainAction::AddCheck { constraint } = &actions[0].action else {
        panic!("CHECK");
    };
    assert_eq!(constraint.name.as_deref(), Some("Positive"));
    assert!(!constraint.validated);
    assert!(constraint.catalog_identity.is_none());
    assert!(matches!(
        &actions[1].action,
        AlterDomainAction::AddCheck { constraint } if constraint.validated && constraint.name.is_none()
    ));
    assert!(matches!(
        &actions[2].action,
        AlterDomainAction::AddNotNull { constraint } if constraint.name.as_deref() == Some("required")
    ));
    assert!(matches!(
        &actions[3].action,
        AlterDomainAction::DropConstraint { name, if_exists: true, cascade: true } if name == "positive"
    ));
    assert!(matches!(
        &actions[4].action,
        AlterDomainAction::ValidateConstraint { name } if name == "positive"
    ));
}

#[test]
fn domain_check_ast_retains_legacy_dispatch_upgrade() {
    let mut statement = first("ALTER DOMAIN d ADD CHECK (true)");
    let Statement::AlterDomain(alter) = &mut statement else {
        panic!("ALTER DOMAIN");
    };
    let AlterDomainAction::AddCheck { constraint } = &mut alter.action else {
        panic!("CHECK");
    };
    constraint.expression = Expr::Func {
        name: "__is_distinct".into(),
        binding: None,
        args: vec![Expr::Literal(Value::Null), Expr::Literal(Value::Int(1))],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    let mut restored: Statement =
        serde_json::from_str(&serde_json::to_string(&statement).unwrap()).unwrap();
    assert!(restored.upgrade_legacy_serialized_dispatches());
    assert!(!restored.upgrade_legacy_serialized_dispatches());
    let Statement::AlterDomain(alter) = restored else {
        unreachable!();
    };
    let AlterDomainAction::AddCheck { constraint } = alter.action else {
        unreachable!();
    };
    assert!(matches!(
        constraint.expression,
        Expr::Func { binding: Some(binding), .. }
            if binding.dispatch == Some(FunctionDispatch::IsDistinct)
    ));
}
