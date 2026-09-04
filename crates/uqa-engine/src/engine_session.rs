//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable views and logical-session catalog/runtime state.

use super::{
    Arc, BTreeMap, CatalogFacade, CatalogIndexRow, ColumnStatsInput, DocId, DocumentStore, Engine,
    Ordering, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult, StoredView,
    StoredViewKind, TableState, Value, ViewRow,
};
use uqa_execution::ScalarExpr;
use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};

type AnalyzeValues = BTreeMap<String, Vec<Value>>;
type AnalyzeNullCounts = BTreeMap<String, u64>;

mod analyze;
mod analyze_helpers;
mod portals;
mod schemas;
mod settings;
mod settings_parse;
mod view_binding;
mod views;
pub(crate) use schemas::is_virtual_system_schema;
pub(crate) use views::{catalog_view_row, MaterializedViewRegistration, ViewRegistration};

use analyze_helpers::{build_histogram, build_mcv, collect_analyze_values, distinct_count};
use settings_parse::parse_search_path_list;
pub(crate) use view_binding::{bind_query_plan_relations, canonical_virtual_relation_reference};
use view_binding::{
    bind_query_plan_sequence_references, query_plan_references_relation,
    query_plan_references_sequence,
};
pub(crate) use view_binding::{function_binding_matches, rewrite_query_plan_routine_identity};

#[cfg(test)]
use view_binding::sequence_function_reference_mut;

#[cfg(test)]
mod tests;
