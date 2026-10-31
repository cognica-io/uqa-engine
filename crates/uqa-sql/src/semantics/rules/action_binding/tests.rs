//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnDef, ColumnType};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, ProductionVec},
    CancellationToken, Value,
};

mod returning;

struct Catalog;
impl RuleSourceCatalog for Catalog {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        Ok(Some(vec!["n".into(), "a".into()]))
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<ColumnDef>, SQLError> {
        Ok(vec![
            ColumnDef::nullable("n", ColumnType::Integer),
            ColumnDef::nullable("a", ColumnType::Integer),
        ])
    }
    fn query_source_columns_with_control(
        &self,
        _: &str,
        _: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        let mut columns = ProductionVec::new(*control);
        for name in ["n", "a"] {
            columns.push_produced(control.copy_text(name)?)?;
        }
        Ok(Some(columns.finish()?))
    }
}

#[derive(Default)]
struct Resolver<'a> {
    calls: Cell<usize>,
    failure: Option<usize>,
    panic_at: Option<usize>,
    cancel_at: Option<(usize, &'a CancellationToken)>,
    expansion: Option<&'a [Expr]>,
    expanded_address: Cell<usize>,
}
impl VariableResolver for Resolver<'_> {
    fn expand_with_control(
        &mut self,
        _: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<Expr>>>, SQLError> {
        control.check()?;
        let Some(expressions) = self.expansion else {
            return Ok(None);
        };
        let mut values = ProductionVec::new(*control);
        for expression in expressions {
            values.push_produced(crate::plan::copy::copy_syntax_with_control(
                expression, control,
            )?)?;
        }
        let values = values.finish()?;
        if let Expr::Literal(Value::Str(text)) = &values[0] {
            self.expanded_address.set(text.as_ptr() as usize);
        }
        Ok(Some(values))
    }
    fn resolve_name(&mut self, _: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_qualified(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn resolve_param(&mut self, _: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
    fn rewrite_with_control(
        &mut self,
        _: RewriteReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Expr>>, SQLError> {
        let next = self.calls.get() + 1;
        self.calls.set(next);
        assert_ne!(self.panic_at, Some(next), "resolver unwind");
        if let Some((at, token)) = self.cancel_at {
            if next == at {
                token.cancel();
            }
        }
        control.check()?;
        if self.failure == Some(next) {
            return Err(SQLError::UnknownColumn("first callback error".into()));
        }
        Ok(None)
    }
}

fn statement(sql: &str) -> Statement {
    crate::compile(sql).unwrap().remove(0)
}

#[test]
fn bound_actions_keep_exact_destination_leases_after_scope_release() {
    let columns = BTreeSet::from(["n".to_string(), "a".to_string()]);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let memory = MemoryBudget::new(4 * 1024 * 1024);
    let sibling = memory.reserve(37).unwrap();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for sql in [
        "SELECT CASE WHEN new.n > 0 THEN ARRAY[1, 2] ELSE ARRAY[3] END, row(new.n, old.n), sum(n) OVER (ORDER BY n ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM items WHERE new.n IN (SELECT n FROM other) ORDER BY new.n LIMIT 3",
        "WITH q AS (SELECT n FROM items) SELECT new.n, n FROM q JOIN other USING (n) UNION ALL SELECT old.n, a FROM items",
        "INSERT INTO items (n, a[1:2]) VALUES (new.n, ARRAY[1,2]) ON CONFLICT (n) DO UPDATE SET a[1] = old.n WHERE items.n = old.n RETURNING n",
        "UPDATE items SET (n, a) = (SELECT new.n, old.n) FROM other WHERE items.n = other.n RETURNING new.n",
        "DELETE FROM items USING other WHERE items.n = old.n RETURNING n",
        "NOTIFY channel, 'payload'",
    ] {
        let action = statement(sql);
        let expected = bind_rule_action(&Catalog, &action, &columns, &mut Resolver::default()).unwrap();
        let mut result = None;
        let observed = allocation_counter::measure(|| {
            result = Some(bind_rule_action_with_control(&Catalog, &action, &columns, &mut Resolver::default(), &control).unwrap());
        });
        let result = result.unwrap();
        assert_eq!(serde_json::to_value(&*result).unwrap(), serde_json::to_value(&expected).unwrap(), "{sql}");
        assert_eq!(usize::try_from(observed.bytes_current).unwrap(), result.reserved_bytes(), "{sql}: {observed:?}");
        assert_eq!(memory.used(), sibling.bytes() + result.reserved_bytes(), "{sql}");
        drop(result);
        assert_eq!(memory.used(), sibling.bytes());
    }
}

#[test]
fn an_early_binding_error_does_not_copy_unused_action_payloads() {
    let mut action = statement("UPDATE items SET n = $1, a = 'payload' RETURNING n");
    let Statement::Update(update) = &mut action else {
        panic!("UPDATE");
    };
    update.assignments[1].1 = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let legacy = allocation_counter::measure(|| drop(action.clone()));
    assert!(legacy.bytes_total >= 4 * 1024 * 1024);
    let columns = BTreeSet::from(["n".into(), "a".into()]);
    let memory = MemoryBudget::new(16 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let observed = allocation_counter::measure(|| {
        let error = bind_rule_action_with_control(
            &Catalog,
            &action,
            &columns,
            &mut Resolver {
                failure: Some(1),
                ..Resolver::default()
            },
            &control,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            SQLError::UnknownColumn("first callback error".into()).to_string()
        );
    });
    assert!(observed.bytes_total < 16 * 1024, "{observed:?}");
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_rule_replacements_are_admitted_before_copying() {
    use crate::semantics::rules::binding::RuleQueryResolver;
    let large = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
    let mut resolver = RuleQueryResolver {
        table: "items".into(),
        columns: vec![("n".into(), Ok(large))],
        old: std::collections::BTreeMap::new(),
        expressions: std::collections::BTreeMap::new(),
    };
    let action = statement("SELECT new.n, new.missing");
    let memory = MemoryBudget::new(16 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let observed = allocation_counter::measure(|| {
        let error = bind_rule_action_with_control(
            &Catalog,
            &action,
            &BTreeSet::new(),
            &mut resolver,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(observed.bytes_total < 16 * 1024, "{observed:?}");
    assert_eq!(memory.used(), 0);
    resolver.columns[0].1 = Ok(Expr::Literal(Value::Int(7)));
    let error =
        bind_rule_action_with_control(&Catalog, &action, &BTreeSet::new(), &mut resolver, &control)
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"));
    assert_eq!(memory.used(), 0);
}

#[test]
fn partial_action_destinations_release_on_each_cancel_error_and_unwind() {
    let action = statement("SELECT repeat('kept', 3), $1, $2, $3");
    let memory = MemoryBudget::new(32 * 1024);
    let sibling = memory.reserve(53).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for token in [&original, &invoking] {
        for at in [1, 3] {
            let mut resolver = Resolver {
                cancel_at: Some((at, token)),
                ..Resolver::default()
            };
            let error = bind_rule_action_with_control(
                &Catalog,
                &action,
                &BTreeSet::new(),
                &mut resolver,
                &control,
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(resolver.calls.get(), at);
            assert_eq!(memory.used(), sibling.bytes());
            token.reset();
        }
    }
    let mut resolver = Resolver {
        failure: Some(3),
        ..Resolver::default()
    };
    assert_eq!(
        bind_rule_action_with_control(&Catalog, &action, &BTreeSet::new(), &mut resolver, &control)
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    assert_eq!(memory.used(), sibling.bytes());
    let mut resolver = Resolver {
        panic_at: Some(3),
        ..Resolver::default()
    };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bind_rule_action_with_control(&Catalog, &action, &BTreeSet::new(), &mut resolver, &control)
    }))
    .is_err());
    assert_eq!(memory.used(), sibling.bytes());
    let recovered = bind_rule_action_with_control(
        &Catalog,
        &action,
        &BTreeSet::new(),
        &mut Resolver::default(),
        &control,
    )
    .unwrap();
    drop(recovered);
    assert_eq!(memory.used(), sibling.bytes());
}

#[test]
fn star_expansion_moves_payloads_and_retires_its_temporary_buffer() {
    let action = statement("SELECT new.*");
    let expressions = [
        Expr::Literal(Value::Str("payload".repeat(1024))),
        Expr::Literal(Value::Int(7)),
    ];
    let memory = MemoryBudget::new(128 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut resolver = Resolver {
        expansion: Some(&expressions),
        ..Resolver::default()
    };
    let mut result = None;
    let observed = allocation_counter::measure(|| {
        result = Some(
            bind_rule_action_with_control(
                &Catalog,
                &action,
                &BTreeSet::new(),
                &mut resolver,
                &control,
            )
            .unwrap(),
        );
    });
    let result = result.unwrap();
    let Statement::Select(query) = &*result else {
        panic!("query");
    };
    assert_eq!(query.projections.len(), 2);
    let Expr::Literal(Value::Str(text)) = &query.projections[0].expr else {
        panic!("payload");
    };
    assert_eq!(text.as_ptr() as usize, resolver.expanded_address.get());
    assert_eq!(
        usize::try_from(observed.bytes_current).unwrap(),
        result.reserved_bytes()
    );
    drop(result);
    assert_eq!(memory.used(), 0);

    let mut action = action;
    let Statement::Select(query) = &mut action else {
        unreachable!();
    };
    query.projections[0].alias = Some("x".repeat(4 * 1024 * 1024));
    let observed = allocation_counter::measure(|| {
        assert_eq!(
            bind_rule_action_with_control(
                &Catalog,
                &action,
                &BTreeSet::new(),
                &mut resolver,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
    });
    assert!(observed.bytes_total < 64 * 1024, "{observed:?}");
    assert_eq!(memory.used(), 0);
}
