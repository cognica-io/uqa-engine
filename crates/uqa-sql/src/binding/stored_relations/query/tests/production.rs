//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{query, BTreeSet, RelationLookupMode, RelationResolution, SQLError};
use crate::binding::stored_relations::{
    bind_stored_query_relations, StoredQueryBindingContext, StoredQuerySequences,
    StoredRelationCatalog,
};
use std::cell::{Cell, RefCell};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

#[derive(Default)]
struct Catalog {
    calls: RefCell<Vec<String>>,
    selected_pointer: Cell<usize>,
}
impl Catalog {
    fn resolve(&self, route: &str, reference: &str) -> RelationResolution {
        self.calls.borrow_mut().push(format!("{route}:{reference}"));
        match reference {
            "missing" => RelationResolution::MissingRelation,
            "absent.item" => RelationResolution::MissingSchema("absent".into()),
            "counter" => RelationResolution::Found("public.counter".into(), "sequence"),
            "composite" => RelationResolution::Found("public.composite".into(), "composite type"),
            "temp" => RelationResolution::Found("\"pg_temp.1\".\"a\"\"b\"".into(), "table"),
            "unqualified" => RelationResolution::Found("item".into(), "table"),
            _ => RelationResolution::Found(format!("public.{reference}"), "table"),
        }
    }
    fn controlled(
        &self,
        route: &str,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        let resolution = self.resolve(route, reference);
        let (resolution, lease) = match resolution {
            RelationResolution::Found(name, kind) => {
                let name = control.copy_text(&name)?;
                self.selected_pointer.set(name.as_ptr() as usize);
                let (name, lease) = name.into_parts();
                (RelationResolution::Found(name, kind), lease)
            }
            RelationResolution::MissingSchema(name) => {
                let (name, lease) = control.copy_text(&name)?.into_parts();
                (RelationResolution::MissingSchema(name), lease)
            }
            RelationResolution::MissingRelation => (resolution, control.empty_reservation()),
        };
        Ok(control.finish(resolution, lease)?)
    }
}
impl StoredRelationCatalog for Catalog {
    fn resolve_age_label_relation_name(&self, reference: &str) -> Result<Option<String>, SQLError> {
        self.calls.borrow_mut().push(format!("age:{reference}"));
        Ok((reference == "age_label").then(|| "pg_temp.1.label".into()))
    }
    fn resolve_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        Ok(self.resolve("visible", reference))
    }
    fn resolve_loaded_visible_relation_kind(
        &self,
        reference: &str,
    ) -> Result<RelationResolution, SQLError> {
        Ok(self.resolve("loaded", reference))
    }
    fn resolve_bound_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        Ok(self.resolve("bound", reference))
    }
    fn resolve_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.controlled("visible", reference, control)
    }
    fn resolve_loaded_visible_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.controlled("loaded", reference, control)
    }
    fn resolve_bound_relation_kind_with_control(
        &self,
        reference: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationResolution>, SQLError> {
        self.controlled("bound", reference, control)
    }
}
impl StoredQuerySequences for Catalog {
    fn query_sequence(&self, reference: &str) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(format!("sequence:{reference}"));
        Err("missing sequence".into())
    }
    fn loaded_query_sequence(&self, reference: &str) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(format!("loaded-sequence:{reference}"));
        Err("missing sequence".into())
    }
}
fn context<'a>(
    catalog: &'a Catalog,
    transitions: &'a BTreeSet<String>,
    mode: RelationLookupMode,
) -> StoredQueryBindingContext<'a> {
    StoredQueryBindingContext {
        relations: catalog,
        lookup_mode: mode,
        sequences: catalog,
        temporary_schema: "pg_temp.1",
        transition_relations: transitions,
    }
}

#[test]
fn stored_query_name_selection_shares_lookup_modes_and_original_diagnostics() {
    let transitions = BTreeSet::from(["a\"b".into(), "age_label".into()]);
    for mode in [RelationLookupMode::Dynamic, RelationLookupMode::Bound] {
        for loaded in [false, true] {
            for reject in [false, true] {
                for reference in [
                    "item",
                    "temp",
                    "unqualified",
                    "\"a\"\"b\"",
                    "age_label",
                    "counter",
                    "composite",
                    "missing",
                    "absent.item",
                ] {
                    let ordinary = Catalog::default();
                    let admitted = Catalog::default();
                    let cancellation = CancellationToken::new();
                    let budget = MemoryBudget::new(1 << 20);
                    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
                    let expected = context(&ordinary, &transitions, mode)
                        .canonical_relation_with_control(
                            reference,
                            "SQL routine body",
                            reject,
                            loaded,
                            &ProductionControl::uncontrolled(),
                        );
                    let actual = context(&admitted, &transitions, mode)
                        .canonical_relation_with_control(
                            reference,
                            "SQL routine body",
                            reject,
                            loaded,
                            &control,
                        );
                    match (expected, actual) {
                        (Ok((expected, expected_temporary)), Ok((actual, actual_temporary))) => {
                            assert_eq!(*actual, *expected, "{reference}");
                            assert_eq!(actual_temporary, expected_temporary, "{reference}");
                            assert_eq!(actual_temporary, reference == "temp", "{reference}");
                            assert_eq!(budget.used(), actual.capacity());
                        }
                        (Err(expected), Err(actual)) => {
                            assert_eq!(actual.sqlstate(), expected.sqlstate(), "{reference}");
                            assert_eq!(actual.to_string(), expected.to_string(), "{reference}");
                        }
                        result => panic!("{reference}: {result:?}"),
                    }
                    assert_eq!(*admitted.calls.borrow(), *ordinary.calls.borrow());
                    let route = match (mode, loaded) {
                        (RelationLookupMode::Bound, _) => "bound",
                        (RelationLookupMode::Dynamic, false) => "visible",
                        (RelationLookupMode::Dynamic, true) => "loaded",
                    };
                    let mut calls = Vec::new();
                    if mode == RelationLookupMode::Dynamic {
                        calls.push(format!("age:{reference}"));
                    }
                    if !matches!(reference, "\"a\"\"b\"" | "age_label") {
                        calls.push(format!("{route}:{reference}"));
                    }
                    assert_eq!(*admitted.calls.borrow(), calls, "{reference}");
                    assert_eq!(budget.used(), 0);
                }
            }
        }
    }
    let catalog = Catalog::default();
    let mut scope = context(&catalog, &transitions, RelationLookupMode::Bound);
    scope.temporary_schema = "public";
    assert!(
        scope
            .canonical_relation_with_control(
                "unqualified",
                "view",
                false,
                true,
                &ProductionControl::uncontrolled()
            )
            .unwrap()
            .1
    );
}

#[test]
fn stored_query_name_selection_transfers_the_catalog_lease_and_rejects_cancelled_work() {
    let catalog = Catalog::default();
    let transitions = BTreeSet::new();
    let scope = context(&catalog, &transitions, RelationLookupMode::Bound);
    let budget = MemoryBudget::new(1 << 20);
    let sibling = budget.reserve(37).unwrap();
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let (name, temporary) = scope
        .canonical_relation_with_control("item", "view", false, true, &control)
        .unwrap();
    assert!(!temporary);
    assert_eq!(name.as_ptr() as usize, catalog.selected_pointer.get());
    assert_eq!(budget.used(), 37 + name.capacity());
    drop(name);
    assert_eq!(budget.used(), 37);
    cancellation.cancel();
    let before = catalog.calls.borrow().len();
    assert_eq!(
        scope
            .canonical_relation_with_control("item", "view", false, true, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(catalog.calls.borrow().len(), before);
    assert_eq!(budget.used(), 37);
    drop(sibling);
    for available in 0.."public.item".len() {
        let budget = MemoryBudget::new(available);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        assert_eq!(
            scope
                .canonical_relation_with_control("item", "view", false, true, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn stored_query_binding_finishes_relation_resolution_before_sequence_errors() {
    for loaded in [false, true] {
        let catalog = Catalog::default();
        let transitions = BTreeSet::new();
        let scope = context(&catalog, &transitions, RelationLookupMode::Bound);
        let mut plan = query("WITH local_rows AS (SELECT * FROM first_source) SELECT nextval('counter') FROM local_rows JOIN second_source ON true");
        let error =
            bind_stored_query_relations(&scope, &mut plan, "SQL routine body", false, loaded)
                .unwrap_err();
        assert!(error.to_string().contains("missing sequence"));
        assert_eq!(
            *catalog.calls.borrow(),
            [
                "bound:first_source",
                "bound:second_source",
                if loaded {
                    "loaded-sequence:counter"
                } else {
                    "sequence:counter"
                }
            ]
        );
        assert!(plan.relations_bound);
    }
    let catalog = Catalog::default();
    let transitions = BTreeSet::new();
    let scope = context(&catalog, &transitions, RelationLookupMode::Bound);
    let mut plan = query("SELECT nextval('counter') FROM missing");
    assert_eq!(
        bind_stored_query_relations(&scope, &mut plan, "SQL routine body", false, true)
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    assert_eq!(*catalog.calls.borrow(), ["bound:missing"]);
}

#[test]
fn owned_query_relation_binding_preserves_cte_scopes_mutations_and_source_order() {
    for sql in [
        "WITH local_rows AS (SELECT * FROM first_source) SELECT * FROM local_rows JOIN second_source ON true",
        "WITH RECURSIVE a AS (SELECT * FROM b), b AS (SELECT * FROM first_source) SELECT * FROM a",
        "WITH changed AS (UPDATE first_source SET value = (SELECT value FROM scalar_source) FROM second_source RETURNING value) SELECT * FROM changed",
        "WITH changed AS (DELETE FROM first_source USING second_source RETURNING *) SELECT * FROM changed",
        "WITH changed AS (INSERT INTO first_source SELECT * FROM second_source RETURNING *) SELECT * FROM changed",
        "SELECT * FROM first_source UNION ALL SELECT * FROM second_source",
        "VALUES ((SELECT value FROM first_source)), ((SELECT value FROM second_source))",
        "SELECT * FROM generate_series(1, 3) AS g JOIN first_source ON true",
        "SELECT * FROM temp",
    ] {
        let input = query(sql);
        let mut expected = input.clone();
        let ordinary = Catalog::default();
        let admitted = Catalog::default();
        let transitions = BTreeSet::new();
        let scope = context(&ordinary, &transitions, RelationLookupMode::Bound);
        let expected_temporary = bind_stored_query_relations(&scope, &mut expected, "view", false, true).unwrap();
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let input = input.copy_for_rewrite(&control).unwrap();
        let scope = context(&admitted, &transitions, RelationLookupMode::Bound);
        let (output, temporary) = scope.bind_query_relation_names_with_control(input, "view", false, true, &control).unwrap();
        assert_eq!(serde_json::to_value(&*output).unwrap(), serde_json::to_value(expected).unwrap(), "{sql}");
        assert_eq!(temporary, expected_temporary, "{sql}");
        assert_eq!(*admitted.calls.borrow(), *ordinary.calls.borrow(), "{sql}");
        assert_eq!(budget.used(), output.reserved_bytes(), "{sql}");
        drop(output);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn owned_query_relation_failure_releases_completed_fields_without_sibling_leases() {
    for sql in [
        "WITH local_rows AS (SELECT * FROM first_source) SELECT * FROM local_rows JOIN missing ON true",
        "WITH changed AS (UPDATE first_source SET value = 1 FROM missing RETURNING *) SELECT * FROM changed",
        "SELECT * FROM first_source UNION ALL SELECT * FROM counter",
    ] {
        let source = query(sql);
        let mut expected = source.clone();
        let ordinary = Catalog::default();
        let admitted = Catalog::default();
        let transitions = BTreeSet::new();
        let expected = bind_stored_query_relations(&context(&ordinary, &transitions, RelationLookupMode::Bound), &mut expected, "view", false, true).unwrap_err();
        let budget = MemoryBudget::new(1 << 20);
        let sibling = budget.reserve(37).unwrap();
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let input = source.copy_for_rewrite(&control).unwrap();
        let actual = context(&admitted, &transitions, RelationLookupMode::Bound).bind_query_relation_names_with_control(input, "view", false, true, &control).unwrap_err();
        assert_eq!(actual.sqlstate(), expected.sqlstate(), "{sql}");
        assert_eq!(actual.to_string(), expected.to_string(), "{sql}");
        assert_eq!(*admitted.calls.borrow(), *ordinary.calls.borrow(), "{sql}");
        assert_eq!(budget.used(), 37, "{sql}");
        drop(sibling);
    }
}

#[test]
fn owned_query_relation_admission_retires_partial_names_and_keeps_other_reservations() {
    let source = query("WITH local_rows AS (SELECT * FROM first_source) SELECT * FROM local_rows JOIN second_source ON true");
    let original = serde_json::to_value(&source).unwrap();
    let mut denied = 0;
    let mut completed = 0;
    for available in 0..512 {
        let budget = MemoryBudget::new(1 << 20);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let sibling = budget.reserve(37).unwrap();
        let input = source.copy_for_rewrite(&control).unwrap();
        let blocked_bytes = (1 << 20) - budget.used() - available;
        let blocker = budget.reserve(blocked_bytes).unwrap();
        let catalog = Catalog::default();
        let transitions = BTreeSet::new();
        let scope = context(&catalog, &transitions, RelationLookupMode::Bound);
        match scope.bind_query_relation_names_with_control(input, "view", false, true, &control) {
            Ok((query, temporary)) => {
                assert!(!temporary);
                assert!(query.relations_bound);
                assert_eq!(budget.used(), blocked_bytes + 37 + query.reserved_bytes());
                completed += 1;
            }
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                denied += 1;
            }
        }
        assert_eq!(budget.used(), blocked_bytes + 37);
        assert_eq!(serde_json::to_value(&source).unwrap(), original);
        drop(blocker);
        assert_eq!(budget.used(), 37);
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
    assert!(
        denied > 0 && completed > 0,
        "{denied} denied, {completed} completed"
    );
}
