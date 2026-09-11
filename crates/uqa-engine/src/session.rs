//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable views and logical-session catalog/runtime state.

use super::{
    Arc, BTreeMap, CatalogFacade, CatalogIndexRow, ColumnStatsInput, DocId, DocumentStore, Engine,
    Ordering, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult, StoredView,
    StoredViewKind, TableState, Value,
};
#[cfg(test)]
use uqa_planner::QueryPlan;

type AnalyzeValues = BTreeMap<String, analyze_helpers::ColumnAnalyzeValues>;
type AnalyzeNullCounts = BTreeMap<String, u64>;

mod analyze;
mod analyze_helpers;
mod clocks;
mod portals;
pub(crate) use portals::StatementReadSnapshot;
mod schemas;
mod settings;
mod settings_parse;
mod views;
pub(crate) use views::catalog_view_row;

use analyze_helpers::{build_histogram, build_mcv, collect_analyze_values, distinct_count};
use settings_parse::parse_search_path_list;

#[cfg(test)]
use uqa_sql::binding::view_dependencies::sequence_function_reference_mut;

#[cfg(test)]
mod tests;

mod directional_query;
