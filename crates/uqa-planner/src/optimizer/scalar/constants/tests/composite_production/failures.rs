//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{cell::Cell, sync::Arc};
use uqa_sql::{
    expr::composites::{CompositeTypeCatalog, CompositeTypeDescriptor},
    routines::declaration::RoutineTypeCatalog,
};

struct Interrupted<'a> {
    calls: Cell<usize>,
    token: &'a CancellationToken,
    fault: usize,
}

impl CompositeTypeCatalog for Interrupted<'_> {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        self.calls.set(self.calls.get() + 1);
        if self.calls.get() == 2 {
            match self.fault {
                0 => return Err(SQLError::Internal("descriptor failure".into())),
                1 => self.token.cancel(),
                _ => panic!("descriptor unwind"),
            }
        }
        Catalog.composite_type(oid)
    }
}

impl RoutineTypeCatalog for Interrupted<'_> {
    fn try_describe_table(
        &self,
        name: &str,
    ) -> Result<Option<Vec<uqa_sql::ast::ColumnDef>>, String> {
        Catalog.try_describe_table(name)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        Catalog.resolve_catalog_column_type(name)
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        Catalog.resolve_catalog_column_type_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, oid: u32) -> Option<ColumnType> {
        Catalog.resolve_catalog_user_type_by_oid(oid)
    }
    fn require_type_usage(&self, ty: &ColumnType) -> Result<(), SQLError> {
        RoutineTypeCatalog::require_type_usage(&Catalog, ty)
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Catalog.format_type(ty)
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
}

#[test]
fn composite_constructor_releases_partial_output_on_catalog_failure_cancellation_and_unwind() {
    for fault in 0..3 {
        for original_cancel in [false, true] {
            let budget = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&budget, &original, &invoking);
            let retained = control.copy_text("independent owner").unwrap();
            let before = budget.used();
            let catalog = Interrupted {
                calls: Cell::new(0),
                token: if original_cancel {
                    &original
                } else {
                    &invoking
                },
                fault,
            };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fold_composite_constructor(constructor(16384), Some(&catalog), &control)
            }));
            if fault == 2 {
                assert!(result.is_err());
            } else {
                let error = result.unwrap().unwrap_err();
                assert_eq!(
                    error.sqlstate(),
                    Some(if fault == 1 { "57014" } else { "XX000" })
                );
            }
            assert_eq!(catalog.calls.get(), 2);
            assert_eq!(budget.used(), before);
            assert_eq!(&*retained, "independent owner");
            drop(retained);
            assert_eq!(budget.used(), 0);
        }
    }
}
