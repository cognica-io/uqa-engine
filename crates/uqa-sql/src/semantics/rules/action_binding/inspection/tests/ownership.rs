//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::events::StoredRule,
    semantics::rules::action_binding::rule_new_row_columns_with_control,
};
use std::cell::Cell;
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl, ProductionVec},
    CancellationToken,
};

struct ControlledCatalog<'a> {
    column: &'a str,
    reads: Cell<usize>,
    interrupt: Option<&'a CancellationToken>,
    panic_on_second: bool,
    fail_on_second: bool,
}
impl RuleSourceCatalog for ControlledCatalog<'_> {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        panic!("uncontrolled source lookup")
    }
    fn rule_relation_definitions(&self, _: &str) -> Result<Vec<ColumnDef>, SQLError> {
        panic!("uncontrolled target lookup")
    }
    fn query_source_columns_with_control(
        &self,
        _: &str,
        _: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<String>>>, SQLError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == 2 {
            if let Some(token) = self.interrupt {
                token.cancel();
            }
            assert!(!self.panic_on_second, "catalog unwind");
            if self.fail_on_second {
                return Err(SQLError::Routine {
                    sqlstate: "XX005".into(),
                    message: "catalog failure".into(),
                });
            }
        }
        let mut columns = ProductionVec::new(*control);
        columns.push_produced(control.copy_text(self.column)?)?;
        Ok(Some(columns.finish()?))
    }
    fn rule_relation_columns_with_control(
        &self,
        _: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<String>>, SQLError> {
        let mut columns = ProductionVec::new(*control);
        columns.push_produced(control.copy_text(self.column)?)?;
        Ok(columns.finish()?)
    }
}
fn rule(action: &str) -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile(&format!(
        "CREATE RULE r AS ON UPDATE TO target DO ALSO {action}"
    ))
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    StoredRule {
        definition,
        enabled: crate::ast::EventEnableMode::default(),
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: None,
    }
}
fn catalog(column: &str) -> ControlledCatalog<'_> {
    ControlledCatalog {
        column,
        reads: Cell::new(0),
        interrupt: None,
        panic_on_second: false,
        fail_on_second: false,
    }
}

#[test]
fn retained_names_and_scope_buffers_match_physical_allocation() {
    let request = rule("SELECT new.n, new.a FROM a AS x NATURAL JOIN b AS y");
    let catalog = catalog("n");
    let memory = MemoryBudget::new(1024 * 1024);
    let sibling = memory.reserve(37).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = rule_new_row_columns_with_control(&catalog, &request, &control).unwrap();
    });
    let output = output.unwrap();
    assert_eq!(&*output, &["a", "n"]);
    assert_eq!(
        usize::try_from(measured.bytes_current).unwrap(),
        memory.used() - sibling.bytes()
    );
    assert_eq!(
        memory.used() - sibling.bytes(),
        output.capacity() * size_of::<String>()
            + output.iter().map(String::capacity).sum::<usize>()
    );
    drop(output);
    assert_eq!(memory.used(), 37);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn source_and_required_names_are_rejected_before_large_copies() {
    let large = "x".repeat(4 * 1024 * 1024);
    let request = rule("SELECT new.n FROM a");
    let mut required = rule("SELECT new.n");
    let Statement::Select(query) = &mut required.definition.actions[0] else {
        panic!("query")
    };
    query.projections[0].expr = Expr::QualifiedColumn {
        qualifier: "new".into(),
        column: large.clone().into(),
    };
    let memory = MemoryBudget::new(512);
    let sibling = memory.reserve(37).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for (request, column) in [(&request, large.as_str()), (&required, "n")] {
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                rule_new_row_columns_with_control(&catalog(column), request, &control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(measured.bytes_total < 16 * 1024, "{measured:?}");
        assert_eq!(memory.used(), sibling.bytes());
    }
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            rule_new_row_columns_with_control(&catalog("n"), &request, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 37);
        token.reset();
    }
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[test]
fn partial_scope_owners_release_on_catalog_failure_cancel_and_unwind() {
    let request = rule("WITH x AS (SELECT * FROM a) SELECT new.n FROM x JOIN b ON true");
    let memory = MemoryBudget::new(1024 * 1024);
    let sibling = memory.reserve(37).unwrap();
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    for token in [&original, &invoking] {
        let catalog = ControlledCatalog {
            interrupt: Some(token),
            ..catalog("n")
        };
        assert_eq!(
            rule_new_row_columns_with_control(&catalog, &request, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 37);
        token.reset();
    }
    let failing = ControlledCatalog {
        fail_on_second: true,
        ..catalog("n")
    };
    assert_eq!(
        rule_new_row_columns_with_control(&failing, &request, &control)
            .unwrap_err()
            .sqlstate(),
        Some("XX005")
    );
    assert_eq!(memory.used(), 37);
    let panicking = ControlledCatalog {
        panic_on_second: true,
        ..catalog("n")
    };
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rule_new_row_columns_with_control(&panicking, &request, &control)
    }))
    .is_err());
    assert_eq!(memory.used(), 37);
    let output = rule_new_row_columns_with_control(&catalog("n"), &request, &control)
        .unwrap()
        .unwrap();
    assert_eq!(&*output, &["n"]);
    drop(output);
    assert_eq!(memory.used(), sibling.bytes());
    drop(sibling);
    assert_eq!(memory.used(), 0);
}
