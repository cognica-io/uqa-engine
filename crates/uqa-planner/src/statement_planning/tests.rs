//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    statistics::CatalogSourceStatistics, PlannerStatisticsCatalog, RetrievalSourceCosting,
    StatementStatisticsContext, StatisticsTableState,
};
use crate::{ColumnStats, LocalAccessEstimate, SourceStatistics};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::{Arc, Weak},
};
use uqa_core::catalog_index::CatalogIndexRow;
use uqa_sql::{
    ast::{FunctionBinding, FunctionVolatility, OperatorJoinRelations},
    plan::QueryPlan,
    semantics::volatility::VolatilityCatalog,
    SQLError, ScalarExpr,
};

struct TableGeneration;
impl StatisticsTableState for TableGeneration {}

struct FailingCatalog {
    table: RefCell<Option<Arc<TableGeneration>>>,
    retained: Weak<TableGeneration>,
    reads: RefCell<Vec<&'static str>>,
    cancel: Option<uqa_core::CancellationToken>,
}

impl PlannerStatisticsCatalog for FailingCatalog {
    fn storage_table(&self, _: &str) -> Result<Option<Arc<dyn StatisticsTableState>>, String> {
        self.table
            .borrow_mut()
            .take()
            .map(|table| Some(table as Arc<dyn StatisticsTableState>))
            .ok_or_else(|| "later table lookup failed".into())
    }

    fn hierarchy_scan_tables(&self, _: &str) -> Result<Vec<String>, SQLError> {
        assert!(self.retained.upgrade().is_some());
        self.reads.borrow_mut().push("hierarchy");
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        Err(SQLError::TypeMismatch("hierarchy metadata failed".into()))
    }

    fn table_row_count(&self, _: &str) -> Result<u64, SQLError> {
        unreachable!("hierarchy discovery failed")
    }

    fn column_statistics(&self, _: &str) -> Result<BTreeMap<String, ColumnStats>, String> {
        assert!(self.retained.upgrade().is_some());
        self.reads.borrow_mut().push("columns");
        Err("column metadata failed".into())
    }

    fn resolved_table_name(&self, _: &str) -> Result<Option<String>, SQLError> {
        unreachable!("relation statistics do not resolve index targets")
    }

    fn catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, SQLError> {
        unreachable!("relation statistics do not read indexes")
    }
}

impl VolatilityCatalog for FailingCatalog {
    fn host_function_volatility(&self, _: &str) -> Option<FunctionVolatility> {
        unreachable!("relation statistics do not inspect functions")
    }

    fn routine_volatilities(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
    ) -> Option<Vec<FunctionVolatility>> {
        unreachable!("relation statistics do not inspect functions")
    }

    fn view_query(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        unreachable!("relation statistics do not rewrite views")
    }
}

impl RetrievalSourceCosting for FailingCatalog {
    fn operator_join_access(
        &self,
        _: &str,
        _: Option<&OperatorJoinRelations>,
        _: &[ScalarExpr],
    ) -> Result<LocalAccessEstimate, SQLError> {
        unreachable!("relation statistics do not cost operator joins")
    }

    fn local_access(
        &self,
        _: &str,
        _: &ScalarExpr,
    ) -> Result<Option<LocalAccessEstimate>, SQLError> {
        unreachable!("relation statistics do not cost retrieval access")
    }
}

#[test]
fn relation_statistics_retain_table_generation_and_first_failure() {
    let table = Arc::new(TableGeneration);
    let catalog = FailingCatalog {
        retained: Arc::downgrade(&table),
        table: RefCell::new(Some(table)),
        reads: RefCell::default(),
        cancel: None,
    };
    let error = RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context: StatementStatisticsContext {
            catalog: &catalog,
            volatility: &catalog,
            retrieval: &catalog,
        },
        error: &error,
    };

    assert!(statistics.relation_statistics("items").is_none());
    assert_eq!(*catalog.reads.borrow(), ["hierarchy", "columns"]);
    assert!(catalog.retained.upgrade().is_none());
    assert!(statistics.relation_statistics("other").is_none());
    assert!(matches!(
        error.borrow().as_ref(),
        Some(SQLError::TypeMismatch(message)) if message == "hierarchy metadata failed"
    ));
}

#[test]
fn cost_catalog_failure_precedes_cancellation_observed_after_the_callback() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let budget = MemoryBudget::new(4096);
        let table = Arc::new(TableGeneration);
        let catalog = FailingCatalog {
            retained: Arc::downgrade(&table),
            table: RefCell::new(Some(table)),
            reads: RefCell::default(),
            cancel: Some(if original_cancelled {
                original.clone()
            } else {
                invoking.clone()
            }),
        };
        let plan = uqa_sql::plan::UnifiedPlan::lower(
            uqa_sql::compile("SELECT value FROM items")
                .unwrap()
                .remove(0),
        );
        let error = super::estimate_plan_with_control(
            StatementStatisticsContext {
                catalog: &catalog,
                volatility: &catalog,
                retrieval: &catalog,
            },
            &plan,
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert!(
            matches!(error, SQLError::TypeMismatch(message) if message == "hierarchy metadata failed")
        );
        assert_eq!(budget.used(), 0);
        assert!(catalog.retained.upgrade().is_none());
    }
}
