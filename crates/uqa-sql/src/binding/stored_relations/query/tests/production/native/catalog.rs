//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Sequences {
    catalog: Arc<SharedCatalog>,
    names: Mutex<Vec<usize>>,
    controlled: AtomicUsize,
}
impl Sequences {
    fn new(catalog: Arc<SharedCatalog>) -> Arc<Self> {
        Arc::new(Self {
            catalog,
            names: Mutex::default(),
            controlled: AtomicUsize::new(0),
        })
    }
    fn resolve(
        &self,
        reference: &str,
        loaded: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        self.catalog
            .0
            .lock()
            .unwrap()
            .calls
            .borrow_mut()
            .push(format!(
                "{}:{reference}",
                if loaded {
                    "loaded-sequence"
                } else {
                    "sequence"
                }
            ));
        if reference == "missing_sequence" {
            return Err(SQLError::Unsupported("missing sequence".into()));
        }
        if reference == "denied_sequence" {
            return Err(SQLError::Diagnostic {
                sqlstate: "42501".into(),
                message: "sequence access denied".into(),
                detail: Some("original detail".into()),
                hint: Some("original hint".into()),
            });
        }
        let name = control.format(format_args!("bound.{reference}"))?;
        self.names.lock().unwrap().push(name.as_ptr() as usize);
        Ok(name)
    }
}
impl StoredQuerySequences for Sequences {
    fn query_sequence(&self, reference: &str) -> Result<String, String> {
        self.resolve(reference, false, &ProductionControl::uncontrolled())
            .map(|name| name.into_uncontrolled().unwrap())
            .map_err(|error| match error {
                SQLError::Unsupported(message) => message,
                error => error.to_string(),
            })
    }
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String> {
        self.resolve(reference, true, &ProductionControl::uncontrolled())
            .map(|name| name.into_uncontrolled().unwrap())
            .map_err(|error| match error {
                SQLError::Unsupported(message) => message,
                error => error.to_string(),
            })
    }
    fn query_sequence_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        self.controlled.fetch_add(1, Ordering::Relaxed);
        self.resolve(reference, false, control)
    }
    fn loaded_query_sequence_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        self.controlled.fetch_add(1, Ordering::Relaxed);
        self.resolve(reference, true, control)
    }
}

fn bind_names(
    plan: RetainedExecutablePlan,
    sequences: Arc<Sequences>,
    lookup: (RelationLookupMode, bool),
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Reader {
    let (mode, loaded) = lookup;
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let transitions = BTreeSet::new();
        let context = StoredQueryBindingContext {
            relations: sequences.catalog.as_ref(),
            lookup_mode: mode,
            sequences: sequences.as_ref(),
            temporary_schema: "pg_temp.1",
            transition_relations: &transitions,
        };
        context
            .bind_query_names_from_input(
                plan,
                "SQL routine body",
                false,
                loaded,
                &control,
                &mut input,
            )
            .await
    })
    .unwrap()
}

#[test]
fn retained_catalog_binding_completes_relations_before_sequences_in_each_lookup_mode() {
    let native = MemoryBudget::new(64 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "WITH c AS (SELECT nextval('cte') FROM base) SELECT nextval('root'), (SELECT currval('child') FROM other) FROM c, temp",
        "WITH c AS (UPDATE base SET a=nextval('command') FROM other RETURNING a) SELECT setval('root', 3) FROM c",
        "SELECT nextval('left') FROM base UNION ALL SELECT nextval('right') FROM other",
    ] {
        for mode in [RelationLookupMode::Dynamic, RelationLookupMode::Bound] {
            for loaded in [false, true] {
                let source = statement(sql);
                let ordinary = Sequences::new(Arc::default());
                let transitions = BTreeSet::new();
                let context = StoredQueryBindingContext { relations: ordinary.catalog.as_ref(), lookup_mode: mode, sequences: ordinary.as_ref(), temporary_schema: "pg_temp.1", transition_relations: &transitions };
                let UnifiedPlan::Query(mut expected) = UnifiedPlan::lower(source.as_ref().clone()) else { panic!("query") };
                let expected_temporary = bind_stored_query_relations(&context, &mut expected, "SQL routine body", false, loaded).unwrap();
                let sequences = Sequences::new(Arc::default());
                let ((actual, temporary), polls) = complete(&mut bind_names(construct(source, &native, &work), sequences.clone(), (mode, loaded), &native, &work, &token, &token));
                assert!(polls > 1);
                assert_eq!(temporary, expected_temporary);
                assert_eq!(observed(&actual), serde_json::to_value(&*expected).unwrap(), "{sql}");
                assert_eq!(*sequences.catalog.0.lock().unwrap().calls.borrow(), *ordinary.catalog.0.lock().unwrap().calls.borrow(), "{sql}");
                assert_eq!(sequences.controlled.load(Ordering::Relaxed), ordinary.names.lock().unwrap().len());
                let mut pointers = Vec::new();
                actual.as_plan_ref().visit_scalar_expressions(&mut |expression| expression.visit(&mut |node| {
                    if let crate::ScalarExpr::Literal(uqa_core::Value::Str(name)) = node { if name.starts_with("bound.") { pointers.push(name.as_ptr() as usize); } }
                }));
                assert!(sequences.names.lock().unwrap().iter().all(|pointer| pointers.contains(pointer)));
                assert_eq!(allocation_counter::measure(|| drop(actual)).count_total, 0);
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        }
    }
}

#[test]
fn retained_catalog_binding_preserves_relation_and_sequence_failure_precedence() {
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, expected_state, lookups) in [
        ("SELECT nextval('missing_sequence') FROM missing", "42P01", 0),
        ("WITH c AS (SELECT nextval('first')) SELECT nextval('missing_sequence'), nextval('last') FROM c", "0A000", 2),
        ("SELECT nextval('denied_sequence'), nextval('last') FROM base", "42501", 1),
    ] {
        let sequences = Sequences::new(Arc::default());
        let error = bind_names(construct(statement(sql), &native, &work), sequences.clone(), (RelationLookupMode::Dynamic, false), &native, &work, &token, &token).read(&mut std::io::empty()).unwrap_err();
        assert_eq!(error.sqlstate(), Some(expected_state), "{error}");
        assert_eq!(sequences.controlled.load(Ordering::Relaxed), lookups);
        if expected_state == "0A000" {
            assert!(matches!(error, SQLError::Unsupported(message) if message == "SQL routine body sequence reference `missing_sequence`: missing sequence"));
        } else if expected_state == "42501" {
            assert!(matches!(error, SQLError::Diagnostic { detail: Some(detail), hint: Some(hint), .. } if detail == "original detail" && hint == "original hint"));
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_catalog_binding_retires_every_interrupted_composed_prefix() {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = statement("WITH c AS (UPDATE base SET a=nextval('cte') RETURNING a) SELECT nextval('root'), (SELECT nextval('child') FROM other) FROM c");
    let make = |original: &CancellationToken, invoking: &CancellationToken| {
        bind_names(
            construct(source.clone(), &native, &work),
            Sequences::new(Arc::default()),
            (RelationLookupMode::Bound, true),
            &native,
            &work,
            original,
            invoking,
        )
    };
    let (output, polls) = complete(&mut make(&token, &token));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancellation in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = make(&original, &invoking);
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
            assert_eq!((native.used(), work.used()), (37, 0));
        }
    }
    drop(sibling);
}
