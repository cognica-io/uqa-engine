//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::mutation::constraints::context::MutationRead;
use crate::scalar::plan::{eval_physical, PhysicalEvalContext};
use crate::schema::columns::{addition::deferred::AddedColumnRows, ColumnRewritePublication};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::{
    catalog_role::RoleIdentity, ArrayValue, CancellationToken, DocId, RelationIdentity,
};
use uqa_sql::{
    assignment::{columns::AssignmentColumnCatalog, AssignmentContext},
    ast::{ColumnDef, CreateDomain, DomainCheck, FunctionBinding, FunctionVolatility},
    catalog::domain::{DomainCatalog, StoredDomain},
    expr::EngineHook,
    plan::{ExpressionPlan, QueryPlan},
    semantics::partition::PartitionExpressions,
    ResultRow, SQLParam,
};
use uqa_storage::document_store::Document;

struct Fixture {
    domain: StoredDomain,
    checks: AtomicUsize,
    evaluations: AtomicUsize,
    writes: Mutex<BTreeMap<DocId, Value>>,
    cancellation: CancellationToken,
}

impl Fixture {
    fn new() -> Self {
        Self {
            domain: StoredDomain {
                object_id: [3; 16],
                oid: 16_384,
                array_oid: Some(16_385),
                identity: RelationIdentity::new("public", "checked"),
                owner: RoleIdentity::BOOTSTRAP,
                definition: CreateDomain {
                    name: "public.checked".into(),
                    base: ColumnType::Integer,
                    collation: None,
                    default: None,
                    not_null: None,
                    checks: vec![DomainCheck {
                        name: Some("observed".into()),
                        catalog_identity: None,
                        expression: Expr::Literal(Value::Bool(true)),
                        validated: true,
                    }],
                },
                array_name: None,
                usage_acl: None,
            },
            checks: AtomicUsize::new(0),
            evaluations: AtomicUsize::new(0),
            writes: Mutex::new(BTreeMap::new()),
            cancellation: CancellationToken::new(),
        }
    }

    fn target(&self) -> ColumnType {
        ColumnType::Array(Box::new(self.domain.column_type()))
    }

    fn resolve(&self, name: &str) -> Option<ColumnType> {
        [self.domain.column_type(), self.target()]
            .into_iter()
            .find(|ty| ty.catalog_name() == name || ty.sql_name() == name)
    }

    fn context(&self) -> ColumnBackfillContext<'_> {
        ColumnBackfillContext {
            rewrite: ColumnRewriteContext {
                cancellation: &self.cancellation,
                columns: self,
                reads: self,
                types: self,
                expressions: self,
                writes: self,
            },
            state: self,
            volatility: self,
            input_types: self,
        }
    }
}

fn array() -> Value {
    Value::Array(
        ArrayValue::with_lower_bounds(vec![Value::Int(1), Value::Int(2)], vec![-1]).unwrap(),
    )
}

impl EngineHook for Fixture {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("the fixture observes checks directly")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!("the fixture observes checks directly")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!("the fixture observes checks directly")
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        Ok(self.resolve(name))
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        uqa_sql::assignment::domain::cast_domain_value(self, value, source, target)
    }
    fn call_scalar_function(&self, name: &str, _: &[Value]) -> Option<Result<Value, SQLError>> {
        (name == "volatile_input").then(|| Ok(array()))
    }
}

impl DomainCatalog for Fixture {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        (oid == self.domain.oid).then(|| self.domain.clone())
    }
}

impl AssignmentContext for Fixture {
    fn evaluate_domain_check(
        &self,
        _: &Expr,
        _: &ResultRow,
        _: &RowSchema,
    ) -> Result<Value, SQLError> {
        self.checks.fetch_add(1, Ordering::Relaxed);
        Ok(Value::Bool(true))
    }
}

impl FunctionTypeResolver for Fixture {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(self.resolve(name))
    }
    fn resolve_function_type(
        &self,
        name: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok((name == "volatile_input").then(|| self.target()))
    }
}

impl AssignmentColumnCatalog for Fixture {
    fn try_describe_table(
        &self,
        _: &str,
    ) -> Result<Option<Vec<ColumnDef>>, uqa_sql::assignment::columns::ColumnCatalogError> {
        unreachable!("assignment uses the single-column shape")
    }
    fn columns_declared(
        &self,
        _: &str,
    ) -> Result<bool, uqa_sql::assignment::columns::ColumnCatalogError> {
        Ok(true)
    }
    fn try_column_insert_default_expr(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<Expr>, uqa_sql::assignment::columns::ColumnCatalogError> {
        unreachable!("backfill receives its retained default")
    }
    fn try_column_shape(
        &self,
        _: &str,
        _: &str,
    ) -> Result<
        Option<Option<uqa_sql::assignment::columns::ColumnShape>>,
        uqa_sql::assignment::columns::ColumnCatalogError,
    > {
        Ok(Some(Some(uqa_sql::assignment::columns::ColumnShape {
            ty: self.target(),
            generated: None,
            identity_sequence: None,
        })))
    }
}

impl PartitionExpressions for Fixture {
    fn evaluate_bound(&self, expression: &Expr, params: &[SQLParam]) -> Result<Value, SQLError> {
        self.evaluations.fetch_add(1, Ordering::Relaxed);
        eval_physical(
            &ExpressionPlan::lower(expression.clone()),
            &PhysicalEvalContext::new(None, params).with_function_hook(self),
        )
    }
    fn evaluate_row(
        &self,
        _: &Expr,
        _: &ResultRow,
        _: &RowSchema,
        _: &[SQLParam],
    ) -> Result<Value, SQLError> {
        unreachable!("defaults have no row namespace")
    }
}

impl VolatilityCatalog for Fixture {
    fn host_function_volatility(&self, name: &str) -> Option<FunctionVolatility> {
        (name == "volatile_input").then_some(FunctionVolatility::Volatile)
    }
    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        None
    }
    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
}

impl ColumnBackfillState for Fixture {
    fn column_type(&self, _: &str, _: &str) -> StorageBackendResult<Option<ColumnType>> {
        Ok(Some(self.target()))
    }
    fn clear_missing_values(&self, _: &str) -> Result<(), SQLError> {
        Ok(())
    }
}

impl ColumnRewritePublication for Fixture {
    fn update_fields(
        &self,
        _: &str,
        id: DocId,
        mut values: BTreeMap<String, Value>,
        _: RowUpdateVectors,
    ) -> Result<bool, SQLError> {
        self.writes.lock().insert(id, values.remove("v").unwrap());
        Ok(true)
    }
}

impl MutationRead for Fixture {
    fn table_doc_ids(&self, _: &str) -> Result<Vec<DocId>, SQLError> {
        Ok(vec![1, 2])
    }
    fn live_table_doc_ids(&self, table: &str) -> Result<Vec<DocId>, SQLError> {
        self.table_doc_ids(table)
    }
    fn live_table_doc_id_page(
        &self,
        _: &str,
        _: Option<DocId>,
        _: usize,
        _: &uqa_storage::read_control::StorageReadControl,
    ) -> Result<uqa_core::memory::BudgetedVec<DocId>, SQLError> {
        unreachable!("backfill uses the existing live-row list")
    }
    fn get_document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        unreachable!("backfill assigns the newly added field")
    }
    fn raw_document(&self, _: &str, _: DocId) -> Result<Option<Document>, SQLError> {
        unreachable!("backfill assigns the newly added field")
    }
    fn command_overlay_changes(
        &self,
        _: &str,
    ) -> Result<Option<crate::query::document_changes::DocumentChanges>, SQLError> {
        unreachable!("backfill uses the existing live-row list")
    }
}

#[test]
fn backfill_preserves_input_identity_without_skipping_new_domain_coercion() {
    for (source, expected_checks) in [(None, 0), (Some("integer[]"), 2)] {
        let fixture = Fixture::new();
        let expression = Expr::TypedLiteral {
            value: array(),
            ty: source.map_or_else(|| fixture.target().catalog_name(), str::to_string),
        };
        assert_eq!(
            backfill_added_column(&fixture.context(), "t", "v", Some(&expression), false).unwrap(),
            Some(array())
        );
        assert_eq!(
            *fixture.writes.lock(),
            BTreeMap::from([(1, array()), (2, array())])
        );
        assert_eq!(fixture.evaluations.load(Ordering::Relaxed), 1);
        assert_eq!(fixture.checks.load(Ordering::Relaxed), expected_checks);
    }
}

#[test]
fn text_default_cast_checks_elements_once_before_backfill_assignment() {
    let fixture = Fixture::new();
    let expression = Expr::Cast {
        expr: Box::new(Expr::TypedLiteral {
            value: Value::Str("[-1:0]={1,2}".into()),
            ty: "text".into(),
        }),
        ty: fixture.target().catalog_name(),
    };
    assert_eq!(
        backfill_added_column(&fixture.context(), "t", "v", Some(&expression), false).unwrap(),
        Some(array())
    );
    assert_eq!(fixture.evaluations.load(Ordering::Relaxed), 1);
    assert_eq!(fixture.checks.load(Ordering::Relaxed), 2);
}

#[test]
fn deferred_defaults_retain_types_for_constant_and_volatile_results() {
    let fixture = Fixture::new();
    let pending = AddedColumnRows::default();
    let constant = Expr::TypedLiteral {
        value: array(),
        ty: fixture.target().catalog_name(),
    };
    let volatile = Expr::Func {
        name: "volatile_input".into(),
        binding: None,
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    for expression in [constant, volatile] {
        pending
            .retain(&fixture.context(), "t", "v", Some(expression))
            .unwrap();
    }
    assert_eq!(fixture.evaluations.load(Ordering::Relaxed), 1);
    for retained in pending.take() {
        for _ in 0..2 {
            assert_eq!(retained.evaluate(&fixture.context()).unwrap(), array());
        }
    }
    assert_eq!(fixture.evaluations.load(Ordering::Relaxed), 3);
    assert_eq!(fixture.checks.load(Ordering::Relaxed), 0);
}
