//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native mutation inputs retain source multiplicity, delayed assignments and their exact constructor leases.

use super::*;
use crate::{
    binding::correlation::CorrelationContext,
    semantics::rules::{action_binding::BoundRuleStatement, binding::OwnedMutationRuleInput},
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

fn mutation(command: &CommandPlan) -> RuleMutation<'_> {
    match command {
        CommandPlan::Update(update) => RuleMutation::Update(update),
        CommandPlan::Delete(delete) => RuleMutation::Delete(delete),
        _ => panic!("mutation"),
    }
}

#[test]
fn admitted_mutation_inputs_preserve_target_source_and_action_namespaces() {
    let catalog = super::owned::catalog_fixture();
    let binding = catalog.binding();
    let correlation = CorrelationContext {
        routines: &catalog,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    for sql in [
        "UPDATE items SET a = 3, b = ARRAY[4]",
        "UPDATE items SET a = a + 1 WHERE a > 0",
        "UPDATE items SET a = DEFAULT, b[1] = (SELECT 7)",
        "UPDATE items SET a = src.a FROM log AS src WHERE items.a = src.a",
        "UPDATE items SET a = (SELECT src.a) FROM log AS src WHERE EXISTS (SELECT 1 WHERE src.a > 0)",
        "DELETE FROM items WHERE a > 0",
        "DELETE FROM items USING log AS src WHERE items.a = src.a",
    ] {
        let command = command(sql);
        let statement = mutation(&command);
        let target = crate::binding::analyze_source_plan_schema(&catalog, &statement.target_source(), &[], &binding, None).unwrap();
        let source = statement.source().map(|source| crate::binding::analyze_source_plan_schema(&catalog, source, &[], &binding, None).unwrap());
        for references in [false, true] {
            let mut ordinary = MutationRuleInput::bind(catalog.analysis(), correlation, statement, (&target, source.as_ref()), references, &"caller".into(), &"owner".into()).unwrap();
            let action = crate::compile(if references { "INSERT INTO log(a, b) VALUES (OLD.a, NEW.b)" } else { "SELECT 1" }).unwrap().remove(0);
            let columns = ["a".to_owned(), "b".to_owned()].into_iter().collect();
            let bound = bind_rule_action(&catalog, &action, &columns, &mut ordinary.resolver).unwrap();
            let context = super::owned::normalize_value(&ordinary.source_context);
            let schema = ordinary.source_context_schema.clone();
            let expected = RuleAction::bind(catalog.analysis(), &|_: &str| false, bound, Some(ordinary.finish(None))).unwrap().lower(&|_: &str| false).unwrap();
            let budget = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let mut input = OwnedMutationRuleInput::bind(catalog.analysis(), correlation, statement, (&target, source.as_ref()), references, &"caller".into(), &"owner".into(), &control).unwrap();
            assert_eq!(super::owned::normalize_value(&*input.source_context), context, "{sql}");
            assert_eq!(input.source_context_schema, schema, "{sql}");
            let bound = BoundRuleStatement::bind(&catalog, &action, &columns, &mut input.resolver, &control).unwrap();
            let plan = RuleAction::bind_with_control(catalog.analysis(), &|_: &str| false, &bound, Some(input.finish(None, &control).unwrap()), &control).unwrap().lower_with_control(&|_: &str| false, &control).unwrap().finish(&control).unwrap();
            assert_eq!(super::owned::normalize(&plan), super::owned::normalize(&expected), "{sql}, references={references}");
            drop((plan, bound));
            assert_eq!(budget.used(), 0, "{sql}");
        }
    }
}

#[test]
fn mutation_templates_defer_assignment_errors_until_the_selected_new_field() {
    let catalog = super::owned::catalog_fixture();
    let command = command("UPDATE items SET a[1] = 5");
    let statement = mutation(&command);
    let binding = catalog.binding();
    let correlation = CorrelationContext {
        routines: &catalog,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let target = crate::binding::analyze_source_plan_schema(
        &catalog,
        &statement.target_source(),
        &[],
        &binding,
        None,
    )
    .unwrap();
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut input = OwnedMutationRuleInput::bind(
        catalog.analysis(),
        correlation,
        statement,
        (&target, None),
        true,
        &"caller".into(),
        &"owner".into(),
        &control,
    )
    .unwrap();
    let before = budget.used();
    for (qualifier, column) in [("old", "a"), ("new", "b")] {
        let value = input
            .resolver
            .rewrite_with_control(
                crate::plpgsql::RewriteReference::Qualified { qualifier, column },
                &control,
            )
            .unwrap()
            .unwrap();
        drop(value);
        assert_eq!(budget.used(), before);
    }
    let (mut ordinary, _) = mutation_input(&catalog, statement);
    let expected = ordinary.rewrite_qualified("new", "a").unwrap_err();
    let error = input
        .resolver
        .rewrite_with_control(
            crate::plpgsql::RewriteReference::Qualified {
                qualifier: "new",
                column: "a",
            },
            &control,
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), expected.sqlstate());
    assert_eq!(error.to_string(), expected.to_string());
    assert_eq!(budget.used(), before);
    drop(input);
    assert_eq!(budget.used(), 0);
}

#[test]
fn mutation_input_failures_release_admitted_prefixes_and_preserve_siblings() {
    let catalog = super::owned::catalog_fixture();
    let binding = catalog.binding();
    let correlation = CorrelationContext {
        routines: &catalog,
        catalog: binding.catalog.as_ref(),
        resolution: &binding.resolution,
        scoped_relations: None,
    };
    let command = command("UPDATE items SET a = src.a FROM log AS src WHERE items.a = src.a");
    let statement = mutation(&command);
    let target = RowSchema::with_qualified_types(
        "items",
        vec!["a".into(), "b".into()],
        vec![
            Some(ColumnType::Integer),
            Some(ColumnType::Array(Box::new(ColumnType::Integer))),
        ],
    );
    let source = RowSchema::with_qualified_types(
        "src",
        target.columns().to_vec(),
        target.column_types().to_vec(),
    );
    for limit in [0, 128, 1024, 4096] {
        let parent = MemoryBudget::new(limit + 29);
        let sibling = parent.reserve(29).unwrap();
        let budget = parent.child(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let error = OwnedMutationRuleInput::bind(
            catalog.analysis(),
            correlation,
            statement,
            (&target, Some(&source)),
            true,
            &"caller".into(),
            &"owner".into(),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
    for request in [false, true] {
        let budget = MemoryBudget::new(1 << 20);
        let original = CancellationToken::new();
        let session = CancellationToken::new();
        if request {
            original.cancel();
        } else {
            session.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &session);
        let error = OwnedMutationRuleInput::bind(
            catalog.analysis(),
            correlation,
            statement,
            (&target, Some(&source)),
            true,
            &"caller".into(),
            &"owner".into(),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}
