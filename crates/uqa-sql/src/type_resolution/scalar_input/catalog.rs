//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Record only the catalog lookups actually needed to select an operand's metadata.

use super::{array_input_type, record_field};
use crate::{
    expr::{CatalogCastScope, EngineHook},
    schema::ScalarTypeSchema,
    ColumnType, SQLError, SQLParam, ScalarExpr,
};
use std::cell::RefCell;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RecordFieldType,
};

#[derive(Debug)]
enum Dependency {
    Independent,
    WithoutHook,
    Snapshot(CatalogCastScope),
    Live,
}

/// The catalog identities consulted by a successful array or record type observation.
#[derive(Debug)]
pub struct OperandTypeScope(Dependency);

impl OperandTypeScope {
    pub fn matches(&self, hook: Option<&dyn EngineHook>) -> bool {
        match &self.0 {
            Dependency::Independent => true,
            Dependency::WithoutHook => hook.is_none(),
            Dependency::Snapshot(saved) => hook
                .and_then(EngineHook::catalog_cast_scope)
                .is_some_and(|current| saved.matches(current)),
            Dependency::Live => false,
        }
    }
}

/// Observe all fields of one constructor in the same invoking catalog. Explicit bindings that need no lookup are independent of that catalog.
pub struct ScalarTypeObservation<'a> {
    hook: Option<&'a dyn EngineHook>,
    dependency: Option<RefCell<Dependency>>,
}

impl<'a> ScalarTypeObservation<'a> {
    pub fn new(hook: Option<&'a dyn EngineHook>) -> Self {
        Self {
            hook,
            dependency: Some(RefCell::new(Dependency::Independent)),
        }
    }

    pub fn live(hook: Option<&'a dyn EngineHook>) -> Self {
        Self {
            hook,
            dependency: None,
        }
    }

    fn catalog(&self) -> OperandCatalog<'_> {
        OperandCatalog {
            hook: self.hook,
            dependency: self.dependency.as_ref(),
        }
    }

    pub fn record_field(
        &self,
        expression: &ScalarExpr,
        schema: &dyn ScalarTypeSchema,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Option<RecordFieldType>, SQLError> {
        record_field(expression, schema, params, self.catalog(), control)
    }

    pub fn array_input(
        &self,
        items: &[ScalarExpr],
        schema: &dyn ScalarTypeSchema,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        array_input_type(items, schema, params, self.catalog(), control)
    }

    pub fn finish(self) -> Option<OperandTypeScope> {
        let scope = OperandTypeScope(self.dependency?.into_inner());
        scope.matches(self.hook).then_some(scope)
    }
}

#[derive(Clone, Copy)]
pub(super) struct OperandCatalog<'a> {
    pub(super) hook: Option<&'a dyn EngineHook>,
    dependency: Option<&'a RefCell<Dependency>>,
}

impl<'a> OperandCatalog<'a> {
    pub(super) fn live(hook: Option<&'a dyn EngineHook>) -> Self {
        Self {
            hook,
            dependency: None,
        }
    }

    pub(super) fn observe(
        self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let Some(dependency) = self.dependency else {
            return Ok(());
        };
        let mut saved = dependency.borrow_mut();
        if matches!(*saved, Dependency::Live | Dependency::WithoutHook) {
            return Ok(());
        }
        let Some(hook) = self.hook else {
            *saved = Dependency::WithoutHook;
            return Ok(());
        };
        let current = hook.catalog_cast_scope();
        match (&*saved, current) {
            (Dependency::Independent, Some(scope))
                if crate::compiler::fixed_type_name_with_control(name, control)? =>
            {
                *saved = Dependency::Snapshot(scope.clone());
            }
            (Dependency::Snapshot(prior), Some(scope))
                if prior.matches(scope)
                    && crate::compiler::fixed_type_name_with_control(name, control)? => {}
            _ => *saved = Dependency::Live,
        }
        Ok(())
    }
}
