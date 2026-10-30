//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    assignment::columns::ColumnCatalogError,
    ast::{AssignmentStep, Expr},
    semantics::generated_values::GeneratedValueColumns,
    ScalarExpr,
};
use std::cell::RefCell;
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

struct Catalog {
    columns: Vec<ColumnDef>,
    declared: bool,
    found: bool,
    trace: RefCell<Vec<&'static str>>,
}

impl Catalog {
    fn new() -> Self {
        let crate::Statement::CreateTable(table) = crate::compile("CREATE TABLE t(id integer GENERATED ALWAYS AS IDENTITY, payload integer, computed integer GENERATED ALWAYS AS(payload+1) STORED)").unwrap().remove(0) else { panic!("table") };
        Self {
            columns: table.columns,
            declared: true,
            found: true,
            trace: RefCell::new(Vec::with_capacity(32)),
        }
    }
}

impl AssignmentColumnCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, ColumnCatalogError> {
        panic!("metadata selection must not copy expressions")
    }
    fn columns_declared(&self, _: &str) -> Result<bool, ColumnCatalogError> {
        self.trace.borrow_mut().push("declared");
        Ok(self.declared)
    }
    fn try_column_insert_default_expr(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<Expr>, ColumnCatalogError> {
        panic!("metadata selection must not evaluate defaults")
    }
    fn with_column_metadata_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
        read: &mut dyn FnMut(&[ColumnDef]) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.trace.borrow_mut().push("metadata");
        if self.found {
            read(&self.columns)?;
        }
        Ok(self.found)
    }
}

struct Names<'a> {
    catalog: &'a Catalog,
    names: &'a [&'a str],
}
impl<'a> IntoIterator for Names<'a> {
    type Item = &'a str;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, &'a str>>;
    fn into_iter(self) -> Self::IntoIter {
        self.catalog.trace.borrow_mut().push("names");
        self.names.iter().copied()
    }
}

#[test]
fn assignment_column_validation_preserves_input_and_catalog_observation_order() {
    let mut catalog = Catalog::new();
    let control = ProductionControl::uncontrolled();
    validate_mutation_columns_with_control(
        &catalog,
        "public.t",
        Names {
            catalog: &catalog,
            names: &["payload"],
        },
        "UPDATE",
        &control,
    )
    .unwrap();
    assert_eq!(&*catalog.trace.borrow(), &["metadata", "names"]);
    catalog.trace.borrow_mut().clear();
    let targets: [AssignmentTarget<ScalarExpr>; 2] = ["id".into(), "id".into()];
    let inputs = targets
        .iter()
        .inspect(|_| catalog.trace.borrow_mut().push("target"));
    let error = validate_mutation_targets_with_control(
        &catalog, "public.t", inputs, "INSERT", true, &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42701"));
    assert_eq!(&*catalog.trace.borrow(), &["target", "target"]);
    catalog.trace.borrow_mut().clear();
    let targets = ["a", "b"].map(|name| AssignmentTarget::<ScalarExpr> {
        column: "payload".into(),
        indirection: vec![AssignmentStep::Field(name.into())],
    });
    validate_mutation_targets_with_control(
        &catalog, "public.t", &targets, "UPDATE", false, &control,
    )
    .unwrap();
    assert_eq!(&*catalog.trace.borrow(), &["metadata"]);
    catalog.columns.clear();
    for (found, declared, succeeds, trace) in [
        (true, false, true, vec!["metadata", "declared"]),
        (true, true, false, vec!["metadata", "declared", "names"]),
        (false, true, false, vec!["metadata"]),
    ] {
        catalog.found = found;
        catalog.declared = declared;
        catalog.trace.borrow_mut().clear();
        let result = validate_mutation_columns_with_control(
            &catalog,
            "public.t",
            Names {
                catalog: &catalog,
                names: &["unknown"],
            },
            "UPDATE",
            &control,
        );
        assert_eq!(result.is_ok(), succeeds);
        assert_eq!(*catalog.trace.borrow(), trace);
    }
}

#[test]
fn assignment_declaration_selection_retains_only_admitted_names() {
    let mut catalog = Catalog::new();
    catalog.columns[1].default = Some(Expr::Literal(Value::Str("x".repeat(4 << 20))));
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = control.copy_text("sibling").unwrap();
    let baseline = budget.used();
    let mut selected = None;
    let measured = allocation_counter::measure(|| {
        validate_mutation_columns_with_control(
            &catalog,
            "public.t",
            ["payload"],
            "INSERT",
            &control,
        )
        .unwrap();
        selected = Some((
            declared_column_names_with_control(&catalog, "public.t", &control)
                .unwrap()
                .unwrap(),
            GeneratedValueColumns::of_with_control(&catalog, "public.t", &control).unwrap(),
        ));
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(
        budget.used() - baseline,
        usize::try_from(measured.bytes_current).unwrap()
    );
    let (names, generated) = selected.unwrap();
    assert_eq!(&**names, &["id", "payload", "computed"]);
    assert!(generated.contains("id"));
    assert!(!generated.contains("computed"));
    drop(names);
    assert!(budget.used() > baseline);
    drop(generated);
    assert_eq!(budget.used(), baseline);
    catalog.columns[0].name = "g".repeat(4 << 20);
    let measured = allocation_counter::measure(|| {
        let error = GeneratedValueColumns::of_with_control(&catalog, "public.t", &control)
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(budget.used(), baseline);
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

#[test]
fn generated_column_validation_consumes_targets_then_uses_declaration_order() {
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let generated = GeneratedValueColumns::of_with_control(&catalog, "public.t", &control).unwrap();
    let baseline = budget.used();
    catalog.trace.borrow_mut().clear();
    let targets = [("computed", true), ("id", true)]
        .into_iter()
        .inspect(|_| catalog.trace.borrow_mut().push("supplied"));
    let error = generated
        .validate_insert_with_control(targets, None, &control)
        .unwrap_err();
    assert_eq!(&*catalog.trace.borrow(), &["supplied", "supplied"]);
    let SQLError::Diagnostic {
        sqlstate,
        message,
        detail,
        hint,
    } = error
    else {
        panic!("diagnostic")
    };
    assert_eq!(sqlstate, "428C9");
    assert_eq!(
        message,
        "cannot insert a non-DEFAULT value into column \"id\""
    );
    assert_eq!(
        detail.as_deref(),
        Some("Column \"id\" is an identity column defined as GENERATED ALWAYS.")
    );
    assert_eq!(
        hint.as_deref(),
        Some("Use OVERRIDING SYSTEM VALUE to override.")
    );
    generated
        .validate_insert_with_control(
            [("id", true)],
            Some(crate::ast::OverridingKind::SystemValue),
            &control,
        )
        .unwrap();
    let error = generated
        .validate_insert_with_control(
            [("computed", true)],
            Some(crate::ast::OverridingKind::SystemValue),
            &control,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "cannot insert a non-DEFAULT value into column \"computed\""
    );
    assert_eq!(
        error.detail(),
        Some("Column \"computed\" is a generated column.")
    );
    assert_eq!(error.hint(), None);
    generated
        .validate_update_with_control([("id", true), ("computed", true)], &control)
        .unwrap();
    let error = generated
        .validate_update_with_control([("computed", false), ("id", false)], &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("428C9"));
    assert!(error
        .to_string()
        .starts_with("column \"id\" can only be updated to DEFAULT"));
    assert_eq!(budget.used(), baseline);
    drop(generated);
    assert_eq!(budget.used(), 0);
}

#[test]
fn assignment_validation_bounds_diagnostics_and_releases_cancelled_workspace() {
    let catalog = Catalog::new();
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = control.copy_text("sibling").unwrap();
    let baseline = budget.used();
    let unknown = "u".repeat(4 << 20);
    let measured = allocation_counter::measure(|| {
        let error = validate_mutation_columns_with_control(
            &catalog,
            "public.t",
            [unknown.as_str()],
            "UPDATE",
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(measured.bytes_total < 256 << 10, "{measured:?}");
    assert_eq!(budget.used(), baseline);
    let generated = GeneratedValueColumns::of_with_control(&catalog, "public.t", &control).unwrap();
    let retained = budget.used();
    let target: AssignmentTarget<ScalarExpr> = "payload".into();
    for token in [&original, &invoking] {
        let supplied = [("payload", false), ("id", true)]
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                if index == 1 {
                    token.cancel();
                }
                target
            });
        assert_eq!(
            generated
                .validate_insert_with_control(supplied, None, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), retained);
        let visited = std::cell::Cell::new(0);
        assert_eq!(
            validate_mutation_targets_with_control(
                &catalog,
                "public.t",
                std::iter::once(&target).inspect(|_| visited.set(visited.get() + 1)),
                "UPDATE",
                false,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(visited.get(), 0);
        token.reset();
    }
    drop(generated);
    assert_eq!(budget.used(), baseline);
    assert_eq!(&**sibling, "sibling");
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
