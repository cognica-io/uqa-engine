//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::routines::{context::RoutineExpressions, invocation::context::RoutineCallRetention};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    assignment::{routines::RoutineValueContext, AssignmentContext},
    ast::{ColumnDef, Expr},
    catalog::domain::DomainCatalog,
    expr::EngineHook,
    plan::ExpressionPlan,
    routines::{
        declaration::RoutineTypeCatalog,
        invocation::descriptions::{RoutineCallDescriptions, RoutineDescriptionScope},
        resolution::{RoutineOverloadCatalog, RoutineOverloadContext, RoutineTypeSnapshot},
        RoutineBody,
    },
    SQLParam, Statement,
};

mod prepared_zero_arguments;
mod projection;

struct Catalog {
    function: Arc<SQLUserFunction>,
    lookups: AtomicUsize,
    sources: AtomicUsize,
    targets: AtomicUsize,
    scope: RoutineDescriptionScope,
}

impl Catalog {
    fn new() -> Self {
        let Statement::CreateFunction(mut def) = uqa_sql::compile(
            "CREATE FUNCTION public.counted(a integer) RETURNS integer LANGUAGE SQL AS 'SELECT a'",
        )
        .unwrap()
        .remove(0) else {
            panic!("function")
        };
        def.object_id = Some([12; 16]);
        Self {
            function: Arc::new(SQLUserFunction::new(*def, RoutineBody::Source)),
            lookups: AtomicUsize::new(0),
            sources: AtomicUsize::new(0),
            targets: AtomicUsize::new(0),
            scope: RoutineDescriptionScope::default(),
        }
    }
    fn binding(&self) -> FunctionBinding {
        RoutineOverloadContext { catalog: self }
            .resolve_static_sql_function_match(
                "public.counted",
                None,
                &[None],
                &[Some(ColumnType::Integer)],
                false,
            )
            .unwrap()
            .unwrap()
            .binding()
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn call_bound_user_function_with_descriptions(
        &self,
        binding: &FunctionBinding,
        args: &[(Option<String>, Value)],
        descriptions: &RoutineCallDescriptions,
        control: &ProductionControl<'_>,
    ) -> Option<Result<Value, SQLError>> {
        let current = || Some(self.scope.clone());
        Some(
            resolve_bound_inputs(
                self,
                &RoutineOverloadContext { catalog: self },
                Some(RoutineCallRetention {
                    descriptions,
                    scope: &current,
                    control: *control,
                }),
                binding,
                args,
            )
            .map(|resolved| resolved.unwrap().bound.remove(0)),
        )
    }
}
impl DomainCatalog for Catalog {
    fn domain_by_oid(&self, _: u32) -> Option<uqa_sql::catalog::domain::StoredDomain> {
        None
    }
}
impl AssignmentContext for Catalog {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &uqa_sql::ResultRow,
        _: &uqa_sql::RowSchema,
    ) -> Result<Value, SQLError> {
        unreachable!()
    }
}
impl RoutineValueContext for Catalog {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        self.targets.fetch_add(1, Ordering::Relaxed);
        ColumnType::from_sql_name(name).ok()
    }
}
impl RoutineExpressions for Catalog {
    fn column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        self.sources.fetch_add(1, Ordering::Relaxed);
        ColumnType::from_sql_name(name)
    }
    fn evaluate(&self, _: &Expr) -> Result<Value, SQLError> {
        unreachable!()
    }
    fn evaluate_with_type(&self, _: &Expr) -> Result<(Value, Option<ColumnType>), SQLError> {
        unreachable!()
    }
    fn expression_type(
        &self,
        _: &ExpressionPlan,
        _: &[SQLParam],
    ) -> Result<Option<ColumnType>, SQLError> {
        unreachable!()
    }
}
impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}
impl RoutineOverloadCatalog for Catalog {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::default()
    }
    fn routine_search_path(&self) -> Vec<String> {
        vec!["public".into()]
    }
    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }
    fn lookup_sql_routine_candidates(
        &self,
        _: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        Ok(Some(vec![Arc::clone(&self.function)]))
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        _: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        Some(vec![Arc::clone(&self.function)])
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.lookup_bound_sql_routine_candidates_by_binding(binding)
    }
}

fn repeated_calls(retain: bool) -> (usize, usize, usize) {
    let catalog = Catalog::new();
    let binding = catalog.binding();
    let descriptions = RoutineCallDescriptions::default();
    let scope = RoutineDescriptionScope::default();
    let current = || Some(scope.clone());
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let retained = retain.then_some(RoutineCallRetention {
        descriptions: &descriptions,
        scope: &current,
        control: ProductionControl::new(&budget, &token, &token),
    });
    for value in 0..512 {
        let selected = resolve_bound_inputs(
            &catalog,
            &RoutineOverloadContext { catalog: &catalog },
            retained,
            &binding,
            &[(None, Value::Int(value))],
        )
        .unwrap()
        .unwrap();
        assert!(Arc::ptr_eq(&selected.function, &catalog.function));
        assert_eq!(selected.bound, vec![Value::Int(value)]);
        if retain && value > 0 {
            assert!(matches!(selected.invocation, Cow::Borrowed(_)));
        }
    }
    if retain {
        assert!(descriptions.get(&binding, &scope).is_some());
    }
    drop(descriptions);
    assert_eq!(budget.used(), 0);
    (
        catalog.lookups.load(Ordering::Relaxed),
        catalog.sources.load(Ordering::Relaxed),
        catalog.targets.load(Ordering::Relaxed),
    )
}

#[test]
fn repeated_bound_calls_resolve_identity_sources_and_targets_once() {
    assert_eq!(repeated_calls(false), (512, 1024, 512));
    assert_eq!(repeated_calls(true), (1, 1, 1));
}

#[test]
fn prepared_scalar_calls_reuse_invocation_metadata_and_clones_start_fresh() {
    use crate::{eval_scalar, scalar::PreparedExpressions, ScalarEvalContext, ScalarExpr};
    let catalog = Catalog::new();
    let binding = catalog.binding();
    let prepared = PreparedExpressions::scalar(ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![ScalarExpr::Column("value".into())],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::default(),
        filter: None,
    });
    for value in 0..512 {
        let row = uqa_sql::ResultRow::from([("value".into(), Value::Int(value))]);
        let context = ScalarEvalContext::new(Some(&row), &[])
            .with_function_hook(&catalog)
            .with_function_states(prepared.calls());
        assert_eq!(eval_scalar(&prepared, &context).unwrap(), Value::Int(value));
    }
    assert_eq!(catalog.lookups.load(Ordering::Relaxed), 1);
    assert_eq!(catalog.sources.load(Ordering::Relaxed), 1);
    assert_eq!(catalog.targets.load(Ordering::Relaxed), 1);
    let cloned = prepared.clone();
    let row = uqa_sql::ResultRow::from([("value".into(), Value::Int(1000))]);
    let context = ScalarEvalContext::new(Some(&row), &[])
        .with_function_hook(&catalog)
        .with_function_states(cloned.calls());
    assert_eq!(eval_scalar(&cloned, &context).unwrap(), Value::Int(1000));
    assert_eq!(catalog.lookups.load(Ordering::Relaxed), 2);
    assert_eq!(catalog.sources.load(Ordering::Relaxed), 2);
    assert_eq!(catalog.targets.load(Ordering::Relaxed), 2);
}
