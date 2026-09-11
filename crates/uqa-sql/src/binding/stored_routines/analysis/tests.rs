//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    binding::{context::BindingContext, fixture},
    catalog::resolution::RelationLookupMode,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

#[derive(Default)]
struct Scopes {
    events: Mutex<Vec<String>>,
    active: AtomicBool,
    generation: AtomicUsize,
    fail_binding: bool,
}
struct Scope<'a>(&'a Scopes);
impl Drop for Scope<'_> {
    fn drop(&mut self) {
        assert!(self.0.active.swap(false, Ordering::SeqCst));
        self.0.events.lock().unwrap().push("release".into());
    }
}
impl StatementBindingScope for Scope<'_> {
    fn binding_context(&self) -> Result<BindingContext<'_>, SQLError> {
        assert!(self.0.active.load(Ordering::SeqCst));
        self.0.events.lock().unwrap().push("binding".into());
        if self.0.fail_binding {
            return Err(SQLError::UnknownTable("unavailable_catalog".into()));
        }
        let mut resolution = fixture::resolution(
            vec!["public".into()],
            format!("pg_temp_{}", self.0.generation.load(Ordering::SeqCst)),
        );
        resolution.set_lookup_mode(RelationLookupMode::Bound);
        Ok(BindingContext {
            catalog: fixture::catalog(BTreeMap::new()),
            resolution,
            ctes: BTreeMap::new(),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: BTreeSet::new(),
            scalar_subqueries: &[],
        })
    }
}
impl CatalogRoutineScopes for Scopes {
    fn with_catalog_scope(&self, analyze: StatementAnalysisOperation<'_>) -> Result<(), SQLError> {
        assert!(!self.active.swap(true, Ordering::SeqCst));
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push(format!("scope:{}", self.generation.load(Ordering::SeqCst)));
        let scope = Scope(self);
        analyze(&scope)
    }
}
impl crate::FunctionTypeResolver for Scopes {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&crate::ast::FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        assert!(self.active.load(Ordering::SeqCst));
        Ok(None)
    }
}
impl RoutineResolution for Scopes {}
impl Scopes {
    fn context(&self) -> CatalogRoutineAnalysisContext<'_> {
        CatalogRoutineAnalysisContext {
            scopes: self,
            routines: self,
        }
    }
}
fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
}

#[test]
fn stored_statement_errors_release_the_scope_without_evaluating_constants() {
    let scopes = Scopes::default();
    let result = scopes
        .context()
        .bind_statement(&plan("SELECT missing, 1 / 0"));
    assert!(matches!(result,Err(SQLError::UnknownColumn(name)) if name=="missing"));
    assert_eq!(
        *scopes.events.lock().unwrap(),
        ["scope:1", "binding", "release"]
    );
    assert!(!scopes.active.load(Ordering::SeqCst));
}
#[test]
fn each_catalog_binding_uses_a_fresh_scope_and_preserves_outer_row_types() {
    let scopes = Scopes::default();
    scopes.context().bind_statement(&plan("SELECT 1")).unwrap();
    let mut expression = ExpressionPlan::lower(crate::ast::Expr::Column("event_value".into()));
    let outer = RowSchema::with_types(vec!["event_value".into()], vec![Some(ColumnType::Boolean)]);
    let ty = scopes
        .context()
        .bind_expression(&mut expression, &[], &outer)
        .unwrap();
    assert_eq!(ty, Some(ColumnType::Boolean));
    assert_eq!(
        *scopes.events.lock().unwrap(),
        ["scope:1", "binding", "release", "scope:2", "binding", "release"]
    );
}
#[test]
fn catalog_binding_failure_precedes_expression_type_lookup_and_releases_scope() {
    let scopes = Scopes {
        fail_binding: true,
        ..Scopes::default()
    };
    let mut expression = ExpressionPlan::lower(crate::ast::Expr::Column("missing".into()));
    let error = scopes
        .context()
        .bind_expression(&mut expression, &[], &RowSchema::default())
        .unwrap_err();
    assert!(matches!(error,SQLError::UnknownTable(name) if name=="unavailable_catalog"));
    assert_eq!(
        *scopes.events.lock().unwrap(),
        ["scope:1", "binding", "release"]
    );
    assert!(!scopes.active.load(Ordering::SeqCst));
}
