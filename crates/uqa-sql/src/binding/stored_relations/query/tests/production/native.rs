//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Statement,
    plan::{PlanRef, RetainedExecutablePlan, UnifiedPlan},
};
use std::{
    fmt::Write as _,
    sync::{Arc, Mutex},
};
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Aggregates;
impl crate::plan::AggregateClassifier for Aggregates {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}

#[derive(Default)]
struct SharedCatalog(Mutex<Catalog>);
impl StoredRelationCatalog for SharedCatalog {
    fn resolve_age_label_relation_name(&self, reference: &str) -> Result<Option<String>, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_age_label_relation_name(reference)
    }
    fn resolve_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_visible_relation_kind(reference)
    }
    fn resolve_loaded_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_loaded_visible_relation_kind(reference)
    }
    fn resolve_bound_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_bound_relation_kind(reference)
    }
    fn resolve_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_visible_relation_kind_with_control(reference, control)
    }
    fn resolve_loaded_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_loaded_visible_relation_kind_with_control(reference, control)
    }
    fn resolve_bound_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.0
            .lock()
            .unwrap()
            .resolve_bound_relation_kind_with_control(reference, control)
    }
}
impl StoredQuerySequences for SharedCatalog {
    fn query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("relation pass must finish before sequence binding")
    }
    fn loaded_query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("relation pass must finish before sequence binding")
    }
}

fn construct(
    source: Arc<Statement>,
    native: &MemoryBudget,
    work: &MemoryBudget,
) -> RetainedExecutablePlan {
    let native = native.clone();
    let token = CancellationToken::new();
    ImageDecoder::<_, SQLError>::new(work, &token, |mut reader| async move {
        let original = CancellationToken::new();
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        UnifiedPlan::lower_executable_from_input(&source, &Aggregates, &control, &mut input).await
    })
    .unwrap()
    .read(&mut std::io::empty())
    .unwrap()
}

type Output = (RetainedExecutablePlan, bool);
type Reader = ImageDecoder<Output, SQLError>;

struct Environment {
    catalog: Arc<SharedCatalog>,
    transitions: BTreeSet<String>,
    mode: RelationLookupMode,
    loaded: bool,
    reject: bool,
}
impl Default for Environment {
    fn default() -> Self {
        Self {
            catalog: Arc::default(),
            transitions: BTreeSet::new(),
            mode: RelationLookupMode::Dynamic,
            loaded: false,
            reject: false,
        }
    }
}
fn binding(
    plan: RetainedExecutablePlan,
    env: Environment,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Reader {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let catalog = StoredQueryBindingContext {
            relations: env.catalog.as_ref(),
            lookup_mode: env.mode,
            sequences: env.catalog.as_ref(),
            temporary_schema: "pg_temp.1",
            transition_relations: &env.transitions,
        };
        catalog
            .bind_query_relation_names_from_input(
                plan,
                "SQL routine body",
                env.reject,
                env.loaded,
                &control,
                &mut input,
            )
            .await
    })
    .unwrap()
}
fn advance(reader: &mut Reader) -> Result<Option<Output>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}
fn complete(reader: &mut Reader) -> (Output, usize) {
    for polls in 1..1_000_000 {
        if let Some(value) = advance(reader).unwrap() {
            return (value, polls);
        }
    }
    panic!("retained binding stopped progressing")
}
fn statement(sql: &str) -> Arc<Statement> {
    Arc::new(crate::compile(sql).unwrap().remove(0))
}
fn observed(plan: &RetainedExecutablePlan) -> serde_json::Value {
    match plan.as_plan_ref() {
        PlanRef::Query(value) => serde_json::to_value(value).unwrap(),
        PlanRef::Command(value) => serde_json::to_value(value).unwrap(),
    }
}

#[test]
fn retained_relation_binding_preserves_complete_plans_scopes_and_callback_order() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT * FROM base",
        "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(3, 4))",
        "WITH \"a\"\"b\" AS (SELECT 1) SELECT * FROM \"a\"\"b\"",
        "WITH a AS (SELECT * FROM base), b AS (SELECT * FROM a) SELECT * FROM b, temp",
        "WITH a AS (SELECT * FROM b), b AS (SELECT 1) SELECT * FROM a",
        "WITH RECURSIVE a AS (SELECT 1 UNION ALL SELECT 1 FROM a) SELECT * FROM a",
        "WITH a AS (SELECT (SELECT n FROM b) FROM base), b AS (SELECT 1 n) SELECT * FROM a",
        "WITH a AS (SELECT 1), b AS (WITH c AS (SELECT * FROM a) SELECT * FROM c) SELECT * FROM b",
        "WITH a AS (SELECT 1), b AS (WITH a AS (SELECT * FROM a) SELECT * FROM a) SELECT * FROM b",
        "WITH a AS (UPDATE base SET n=(SELECT n FROM other) FROM side RETURNING n) SELECT * FROM a",
        "WITH a AS (INSERT INTO base SELECT * FROM other RETURNING n) SELECT * FROM a",
        "WITH a AS (DELETE FROM base USING other RETURNING n) SELECT * FROM a",
        "SELECT * FROM (SELECT * FROM base) a JOIN (SELECT * FROM other) b ON true UNION ALL SELECT * FROM temp",
        "VALUES ((SELECT n FROM base)), ((SELECT n FROM other))",
        "SELECT * FROM text_similarity_join(base, 'x', other, 'y', 0.5)",
        "SELECT nextval('counter') FROM base",
    ] {
        for mode in [RelationLookupMode::Dynamic, RelationLookupMode::Bound] {
            for loaded in [false, true] {
                let source = statement(sql);
                let ordinary_catalog = Catalog::default();
                let transitions = BTreeSet::new();
                let UnifiedPlan::Query(mut expected) = UnifiedPlan::lower(source.as_ref().clone()) else { panic!("query") };
                // The established raw relation visitor is independent of the mutable traversal under test.
                let mut temporary = false;
                crate::binding::view_dependencies::bind_query_plan_relations(&mut expected, &BTreeSet::new(), &mut |reference| {
                    let (name, uses_temporary) = context(&ordinary_catalog, &transitions, mode).canonical_relation_with_control(reference, "SQL routine body", false, loaded, &ProductionControl::uncontrolled())?;
                    temporary |= uses_temporary;
                    Ok::<_, SQLError>(name.into_uncontrolled().unwrap())
                }).unwrap();
                let catalog = Arc::new(SharedCatalog::default());
                let plan = construct(source, &native, &work);
                let ((actual, actual_temporary), polls) = complete(&mut binding(plan, Environment { catalog: catalog.clone(), mode, loaded, ..Environment::default() }, &native, &work, &token, &token));
                assert_eq!(observed(&actual), serde_json::to_value(&*expected).unwrap(), "{sql}");
                assert_eq!(actual_temporary, temporary, "{sql}");
                if sql == "SELECT * FROM base" {
                    let PlanRef::Query(query) = actual.as_plan_ref() else { panic!("query") };
                    let crate::plan::RelationalPlan::QueryBlock(block) = &query.root else { panic!("block") };
                    let Some(crate::plan::SourcePlan::Table { name, .. }) = &block.from else { panic!("source") };
                    assert_eq!(name.as_ptr() as usize, catalog.0.lock().unwrap().selected_pointer.get());
                }
                assert_eq!(*catalog.0.lock().unwrap().calls.borrow(), *ordinary_catalog.calls.borrow(), "{sql}");
                assert!(polls > 1);
                assert_eq!(work.used(), 0);
                assert_eq!(allocation_counter::measure(|| drop(actual)).count_total, 0);
                assert_eq!(native.used(), 0);
            }
        }
    }
}

#[test]
fn retained_relation_binding_retires_every_pending_prefix_and_both_cancellations() {
    let source = statement("WITH a AS (SELECT * FROM base), b AS (UPDATE target SET n=(SELECT n FROM other) FROM a RETURNING n) SELECT * FROM b, temp");
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) = complete(&mut binding(
        construct(source.clone(), &native, &work),
        Environment::default(),
        &native,
        &work,
        &token,
        &token,
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let plan = construct(source.clone(), &native, &work);
            let mut reader = binding(
                plan,
                Environment::default(),
                &native,
                &work,
                &original,
                &invoking,
            );
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancellation != 0 {
                if cancellation == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancellation}"
            );
        }
    }
    drop(sibling);
}

#[test]
fn retained_relation_binding_preserves_first_errors_and_releases_denied_names() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, expected_state) in [
        ("SELECT * FROM base, missing, counter", "42P01"),
        ("SELECT * FROM base, counter", "42809"),
        ("SELECT * FROM base, composite", "42809"),
    ] {
        let plan = construct(statement(sql), &native, &work);
        let catalog = Arc::new(SharedCatalog::default());
        let error = binding(
            plan,
            Environment {
                catalog: catalog.clone(),
                ..Environment::default()
            },
            &native,
            &work,
            &token,
            &token,
        )
        .read(&mut std::io::empty())
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(expected_state), "{sql}: {error}");
        if sql.contains("missing") {
            assert!(!catalog
                .0
                .lock()
                .unwrap()
                .calls
                .borrow()
                .iter()
                .any(|call| call.ends_with(":counter")));
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
    let source = statement("WITH a AS (SELECT * FROM base) SELECT * FROM a, other");
    let sibling = native.reserve(37).unwrap();
    let mut failures = 0;
    for available in [0, 1, 512, 4096, 8192, 16384, 32768, 65536] {
        let plan = construct(source.clone(), &native, &work);
        let blocker = native
            .reserve(native.limit() - native.used() - available)
            .unwrap();
        let result = binding(plan, Environment::default(), &native, &work, &token, &token)
            .read(&mut std::io::empty());
        match result {
            Ok(value) => drop(value),
            Err(error) => {
                failures += 1;
                assert_eq!(error.sqlstate(), Some("53200"));
            }
        }
        drop(blocker);
        assert_eq!((native.used(), work.used()), (37, 0));
    }
    assert!(failures >= 3);
    drop(sibling);
}

#[test]
fn retained_relation_binding_bounds_wide_scopes_and_large_decoded_names() {
    let native = MemoryBudget::new(128 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut sql = String::from("WITH ");
    for index in 0..512 {
        if index != 0 {
            sql.push_str(", ");
        }
        write!(sql, "q{index} AS (SELECT 1)").unwrap();
    }
    sql.push_str(" SELECT * FROM q511");
    let source = statement(&sql);
    let catalog = Arc::new(SharedCatalog::default());
    let mut reader = binding(
        construct(source, &native, &work),
        Environment {
            catalog: catalog.clone(),
            ..Environment::default()
        },
        &native,
        &work,
        &token,
        &token,
    );
    let mut polls = 0;
    loop {
        let mut result = None;
        let allocations = allocation_counter::measure(|| {
            result = Some(advance(&mut reader));
        });
        assert!(
            allocations.count_total <= 16,
            "productive binding allocations: {allocations:?}"
        );
        assert!(
            allocations.count_total as i64 - allocations.count_current <= 64,
            "productive binding retirement: {allocations:?}"
        );
        polls += 1;
        if let Some(value) = result.unwrap().unwrap() {
            drop(value);
            break;
        }
    }
    assert!(polls > 512);
    assert!(catalog.0.lock().unwrap().calls.borrow().is_empty());
    assert_eq!((native.used(), work.used()), (0, 0));

    let Statement::Select(mut source) = crate::compile("WITH a AS (SELECT 1) SELECT * FROM a")
        .unwrap()
        .remove(0)
    else {
        panic!("query")
    };
    let long_name = "cte_é🙂\"".repeat(8192);
    source.with[0].name = long_name.clone();
    let Some(crate::ast::FromClause::Table { name, .. }) = source.from.as_mut() else {
        panic!("table")
    };
    *name = format!("\"{}\"", long_name.replace('"', "\"\""));
    let catalog = Arc::new(SharedCatalog::default());
    let ((output, temporary), polls) = complete(&mut binding(
        construct(Arc::new(Statement::Select(source)), &native, &work),
        Environment {
            catalog: catalog.clone(),
            ..Environment::default()
        },
        &native,
        &work,
        &token,
        &token,
    ));
    assert!(!temporary);
    assert!(polls > 100);
    assert!(catalog.0.lock().unwrap().calls.borrow().is_empty());
    drop(output);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_relation_binding_keeps_deep_children_and_failure_cleanup_on_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(128 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            for leaf in ["base", "missing"] {
                let Statement::Select(mut source) =
                    crate::compile(&format!("SELECT * FROM {leaf}"))
                        .unwrap()
                        .remove(0)
                else {
                    panic!("query")
                };
                for _ in 0..512 {
                    let Statement::Select(mut parent) =
                        crate::compile("WITH owned_scope AS (SELECT 1) SELECT 1")
                            .unwrap()
                            .remove(0)
                    else {
                        panic!("query")
                    };
                    parent.projections[0].expr = crate::ast::Expr::ScalarSubquery(source);
                    source = parent;
                }
                let plan = construct(Arc::new(Statement::Select(source)), &native, &work);
                let result = binding(plan, Environment::default(), &native, &work, &token, &token)
                    .read(&mut std::io::empty());
                match result {
                    Ok((value, temporary)) => {
                        assert_eq!(leaf, "base");
                        assert!(!temporary);
                        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
                    }
                    Err(error) => {
                        assert_eq!(leaf, "missing");
                        assert_eq!(error.sqlstate(), Some("42P01"));
                    }
                }
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_relation_binding_preserves_mutation_roots_and_original_target_flags() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "INSERT INTO base SELECT n FROM other RETURNING n",
        "UPDATE ONLY base SET n=(SELECT n FROM other) FROM side RETURNING n",
        "DELETE FROM ONLY base USING other RETURNING n",
        "MERGE INTO base USING other ON base.n=other.n WHEN MATCHED THEN UPDATE SET n=other.n RETURNING base.n",
        "WITH a AS (SELECT n FROM other) INSERT INTO base SELECT n FROM a RETURNING n",
    ] {
        let mut expected = query(&format!("WITH action AS ({sql}) SELECT * FROM action"));
        let ordinary = Catalog::default();
        let transitions = BTreeSet::new();
        bind_stored_query_relations(&context(&ordinary, &transitions, RelationLookupMode::Bound), &mut expected, "SQL routine body", false, true).unwrap();
        let crate::plan::CtePlanBody::Command(expected) = &expected.ctes[0].body else { panic!("mutation") };
        let catalog = Arc::new(SharedCatalog::default());
        let ((actual, temporary), _) = complete(&mut binding(construct(statement(sql), &native, &work), Environment { catalog: catalog.clone(), mode: RelationLookupMode::Bound, loaded: true, ..Environment::default() }, &native, &work, &token, &token));
        assert_eq!(observed(&actual), serde_json::to_value(&**expected).unwrap(), "{sql}");
        assert!(!temporary);
        assert_eq!(*catalog.0.lock().unwrap().calls.borrow(), *ordinary.calls.borrow());
        drop(actual);
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}
