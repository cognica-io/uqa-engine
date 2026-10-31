//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native rule inputs admit declaration, scalar, scope and pruning outputs before allocation.

use super::*;
use crate::{
    plan::RuleValuesInput,
    semantics::rules::{
        action_binding::{rule_action_target_columns_with_control, BoundRuleStatement},
        binding::{OwnedRuleQueryInput, OwnedRuleQueryResolver},
    },
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn expected_action(
    catalog: &Catalog,
    insert: &crate::plan::InsertPlan,
    action: &Statement,
    relation: InternalRelationId,
    columns: &std::collections::BTreeSet<String>,
) -> UnifiedPlan {
    let mut ordinary =
        RuleQueryResolver::from_insert(catalog.analysis(), insert, relation).unwrap();
    let bound = bind_rule_action(catalog, action, columns, &mut ordinary).unwrap();
    RuleAction::bind(
        catalog.analysis(),
        &|_: &str| false,
        bound,
        Some(RuleQueryInput {
            query: (**insert.source.as_ref().unwrap()).clone(),
            inline: insert
                .rule_values_input
                .as_ref()
                .is_some_and(RuleValuesInput::is_inline),
            relation,
            condition: None,
            expressions: ordinary.expressions,
        }),
    )
    .unwrap()
    .lower(&|_: &str| false)
    .unwrap()
}

#[test]
fn admitted_insert_resolvers_preserve_native_actions_defaults_and_inline_queries() {
    let catalog = super::owned::catalog_fixture();
    for sql in [
        "INSERT INTO items (a, b) VALUES (1, ARRAY[2])",
        "INSERT INTO items (a, b) VALUES (1, ARRAY[2]), (DEFAULT, DEFAULT)",
        "INSERT INTO items (a) VALUES ((SELECT 3))",
        "INSERT INTO items (b[1]) VALUES (7)",
        "INSERT INTO items DEFAULT VALUES",
        "INSERT INTO items (a, b) SELECT 11, ARRAY[13]",
    ] {
        let mut command = command(sql);
        let CommandPlan::Insert(insert) = command.as_mut() else {
            panic!("INSERT")
        };
        if let Some(mut values) = RuleValuesInput::describe(insert).unwrap() {
            insert.source = Some(values.source(catalog.analysis(), insert).unwrap());
            values.project_layer(catalog.analysis(), insert).unwrap();
        }
        let action = crate::compile("INSERT INTO log(a, b) VALUES (NEW.a, NEW.b)")
            .unwrap()
            .remove(0);
        let relation = InternalRelationId::allocate();
        let columns = ["a".to_owned(), "b".to_owned()].into_iter().collect();
        let expected = expected_action(&catalog, insert, &action, relation, &columns);
        let budget = MemoryBudget::new(8 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                OwnedRuleQueryResolver::from_insert_with_control(
                    catalog.analysis(),
                    insert,
                    relation,
                    &control,
                )
                .unwrap(),
            );
        });
        let mut resolver = result.unwrap();
        assert_eq!(measured.bytes_current, budget.used() as i64, "{sql}");
        assert_eq!(resolver.reserved_bytes(), budget.used(), "{sql}");
        let columns = rule_action_target_columns_with_control(&catalog, &action, &control).unwrap();
        let required = resolver
            .input_columns_with_control(
                catalog.analysis(),
                insert,
                &action,
                None,
                &*columns,
                &control,
            )
            .unwrap()
            .unwrap();
        assert_eq!(required.width, insert.columns.len(), "{sql}");
        let bound = BoundRuleStatement::bind(&catalog, &action, &*columns, &mut resolver, &control)
            .unwrap();
        let plan = RuleAction::bind_with_control(
            catalog.analysis(),
            &|_: &str| false,
            &bound,
            Some(OwnedRuleQueryInput {
                query: insert
                    .source
                    .as_ref()
                    .unwrap()
                    .copy_for_rewrite(&control)
                    .unwrap(),
                inline: insert
                    .rule_values_input
                    .as_ref()
                    .is_some_and(RuleValuesInput::is_inline),
                relation,
                condition: None,
                expressions: control
                    .finish(
                        std::mem::take(&mut resolver.expressions),
                        control.empty_reservation(),
                    )
                    .unwrap(),
            }),
            &control,
        )
        .unwrap()
        .lower_with_control(&|_: &str| false, &control)
        .unwrap()
        .finish(&control)
        .unwrap();
        assert_eq!(
            super::owned::normalize(&plan),
            super::owned::normalize(&expected),
            "{sql}"
        );
        drop((plan, bound, required, columns, resolver));
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn insert_resolver_admission_and_cancellation_leave_sibling_owners_intact() {
    let mut column = ColumnDef::nullable("a", ColumnType::Text);
    column.default = Some(Expr::Literal(Value::Str("x".repeat(4 << 20))));
    let catalog = Catalog(vec![column]);
    let command = command("INSERT INTO items(a) SELECT 'small'");
    let CommandPlan::Insert(insert) = &*command else {
        panic!("INSERT")
    };
    let budget = MemoryBudget::new(32 << 10);
    let sibling = budget.reserve(37).unwrap();
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let measured = allocation_counter::measure(|| {
        let error = OwnedRuleQueryResolver::from_insert_with_control(
            catalog.analysis(),
            insert,
            InternalRelationId::allocate(),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 128 << 10);
    assert_eq!(budget.used(), 37);
    let catalog = Catalog(vec![ColumnDef::nullable("a", ColumnType::Text)]);
    for cancel_request in [false, true] {
        let request = CancellationToken::new();
        let session = CancellationToken::new();
        if cancel_request {
            request.cancel();
        } else {
            session.cancel();
        }
        let control = ProductionControl::new(&budget, &request, &session);
        assert_eq!(
            OwnedRuleQueryResolver::from_insert_with_control(
                catalog.analysis(),
                insert,
                InternalRelationId::allocate(),
                &control
            )
            .err()
            .unwrap()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 37);
    }
    let resolver = OwnedRuleQueryResolver::from_insert_with_control(
        catalog.analysis(),
        insert,
        InternalRelationId::allocate(),
        &control,
    )
    .unwrap();
    drop(resolver);
    assert_eq!(budget.used(), 37);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

impl crate::binding::statements::StatementBindingScope for Catalog {
    fn binding_context(&self) -> Result<crate::binding::BindingContext<'_>, SQLError> {
        Ok(self.binding())
    }
}
impl crate::binding::stored_routines::analysis::CatalogRoutineScopes for Catalog {
    fn with_catalog_scope(
        &self,
        analyze: crate::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        analyze(self)
    }
}

#[test]
fn admitted_rule_condition_rows_preserve_fields_query_scopes_and_lazy_structure() {
    let catalog = super::owned::catalog_fixture();
    let binding = crate::catalog::events::RuleConditionBinding::for_event(
        &["a".into(), "b".into()],
        RuleEvent::Insert,
    );
    let context = crate::binding::stored_routines::analysis::CatalogRoutineAnalysisContext {
        scopes: &catalog,
        routines: &catalog,
    };
    let mut differences = Vec::new();
    for sql in [
        "NEW",
        "NEW.a",
        "ROW(NEW.*)",
        "(SELECT NEW.a)",
        "(SELECT new FROM (VALUES (5)) AS q(new))",
        "(SELECT q.new FROM (VALUES (5)) AS q(new))",
    ] {
        let Statement::Select(mut syntax) =
            crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
        else {
            panic!("SELECT")
        };
        let original = crate::plan::ExpressionPlan::lower(syntax.projections.remove(0).expr);
        let mut expected = original.clone();
        context
            .bind_rule_inputs(&mut expected, &binding, "items", &catalog.0)
            .unwrap();
        let budget = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let input = original.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                context
                    .bind_rule_inputs_owned(input, &binding, "items", &catalog.0, &control)
                    .unwrap_or_else(|error| panic!("{sql}: {error}"))
                    .finish(&control)
                    .unwrap(),
            );
        });
        let result = result.unwrap();
        assert_eq!(
            before as i64 + measured.bytes_current,
            budget.used() as i64,
            "{sql}"
        );
        let actual = serde_json::to_value(&*result).unwrap();
        let expected = serde_json::to_value(&expected).unwrap();
        if actual != expected {
            differences.push(format!("{sql}: {actual} != {expected}"));
        }
        assert_eq!(result.reserved_bytes(), budget.used());
        drop(result);
        assert_eq!(budget.used(), 0);
    }
    assert!(differences.is_empty(), "{differences:#?}");
}
