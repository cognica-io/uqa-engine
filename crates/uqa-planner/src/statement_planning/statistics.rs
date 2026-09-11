//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog statistics and retrieval access estimates with first-error preservation.

use crate::{ColumnStats, LocalAccessEstimate};
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::catalog_index::CatalogIndexRow;
use uqa_sql::{
    ast::OperatorJoinRelations,
    semantics::volatility::{self, VolatilityCatalog},
    SQLError, ScalarExpr,
};

/// Retain the loaded table generation across its dependent metadata reads.
pub trait StatisticsTableState: Send + Sync {}

/// Loaded metadata needed to estimate relational access without depending on a storage provider.
pub trait PlannerStatisticsCatalog {
    fn storage_table(&self, table: &str) -> Result<Option<Arc<dyn StatisticsTableState>>, String>;
    fn hierarchy_scan_tables(&self, table: &str) -> Result<Vec<String>, SQLError>;
    fn table_row_count(&self, table: &str) -> Result<u64, SQLError>;
    fn column_statistics(&self, table: &str) -> Result<BTreeMap<String, ColumnStats>, String>;
    fn resolved_table_name(&self, table: &str) -> Result<Option<String>, SQLError>;
    fn catalog_indexes(&self) -> Result<Vec<CatalogIndexRow>, SQLError>;
}
/// Access estimates supplied by the retrieval planner selected by the composition boundary.
pub trait RetrievalSourceCosting {
    fn operator_join_access(
        &self,
        name: &str,
        relations: Option<&OperatorJoinRelations>,
        args: &[ScalarExpr],
    ) -> Result<LocalAccessEstimate, SQLError>;
    fn local_access(
        &self,
        table: &str,
        predicate: &ScalarExpr,
    ) -> Result<Option<LocalAccessEstimate>, SQLError>;
}
#[derive(Clone, Copy)]
pub struct StatementStatisticsContext<'a> {
    pub catalog: &'a dyn PlannerStatisticsCatalog,
    pub volatility: &'a dyn VolatilityCatalog,
    pub retrieval: &'a dyn RetrievalSourceCosting,
}
pub(super) struct CatalogSourceStatistics<'a> {
    pub(super) context: StatementStatisticsContext<'a>,
    pub(super) error: &'a std::cell::RefCell<Option<SQLError>>,
}

impl CatalogSourceStatistics<'_> {
    fn record_error(&self, error: SQLError) {
        if self.error.borrow().is_none() {
            *self.error.borrow_mut() = Some(error);
        }
    }
}

impl crate::SourceStatistics for CatalogSourceStatistics<'_> {
    fn relation_statistics(&self, table: &str) -> Option<crate::RelationStats> {
        match self.context.catalog.storage_table(table) {
            Ok(None) => None,
            Ok(Some(_)) => match (
                hierarchy_row_count(self.context.catalog, table),
                self.context.catalog.column_statistics(table),
            ) {
                (Ok(row_count), Ok(columns)) => Some(crate::RelationStats { row_count, columns }),
                (Err(error), _) => {
                    self.record_error(error);
                    None
                }
                (_, Err(error)) => {
                    self.record_error(SQLError::Internal(format!(
                        "read optimizer statistics for `{table}`: {error}"
                    )));
                    None
                }
            },
            Err(error) => {
                self.record_error(SQLError::Internal(format!(
                    "resolve optimizer storage table `{table}`: {error}"
                )));
                None
            }
        }
    }

    fn source_access_estimate(
        &self,
        source: &crate::SourcePlan,
    ) -> Option<crate::LocalAccessEstimate> {
        let crate::SourcePlan::Function {
            name,
            relations,
            args,
            ..
        } = source
        else {
            return None;
        };
        if args.iter().any(|argument| {
            argument.contains_parameter()
                || volatility::expr_contains_volatile_function(self.context.volatility, argument)
        }) {
            return None;
        }
        let identity = name.to_ascii_lowercase();
        let lower = uqa_sql::semantics::builtin_function_dispatch_name(&identity);
        if !uqa_sql::registry::is_operator_join_table_function(&lower) {
            return None;
        }
        match self
            .context
            .retrieval
            .operator_join_access(&lower, relations.as_ref(), args)
        {
            Ok(estimate) => Some(estimate),
            Err(error) => {
                self.record_error(error);
                None
            }
        }
    }

    fn local_access_estimate(
        &self,
        table: &str,
        predicate: &uqa_sql::ScalarExpr,
    ) -> Option<crate::LocalAccessEstimate> {
        if volatility::expr_contains_volatile_function(self.context.volatility, predicate) {
            return None;
        }
        if predicate.contains_parameter() {
            return match super::parameterized::parameterized_access(self, table, predicate) {
                Ok(estimate) => estimate,
                Err(error) => {
                    self.record_error(error);
                    None
                }
            };
        }
        match self.context.catalog.storage_table(table) {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(error) => {
                self.record_error(SQLError::Internal(format!(
                    "resolve optimizer storage table `{table}`: {error}"
                )));
                return None;
            }
        }
        match self.context.retrieval.local_access(table, predicate) {
            Ok(estimate) => estimate,
            Err(error) => {
                self.record_error(error);
                None
            }
        }
    }
}

fn hierarchy_row_count(
    catalog: &dyn PlannerStatisticsCatalog,
    table: &str,
) -> Result<u64, SQLError> {
    let mut total = 0_u64;
    for member in catalog.hierarchy_scan_tables(table)? {
        total = total
            .checked_add(catalog.table_row_count(&member)?)
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "optimizer hierarchy row count overflow for `{table}`"
                ))
            })?;
    }
    Ok(total)
}
