//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable views and logical-session catalog/runtime state.

use super::{
    build_histogram, build_mcv, distinct_count, Arc, BTreeMap, CatalogFacade, CatalogIndexRow,
    ColumnStatsInput, DocId, DocumentStore, Engine, Ordering, RelationIdentity, SQLError,
    StorageBackendError, StorageBackendResult, StoredView, TableState, Value, ViewRow,
};
use uqa_execution::ScalarExpr;
use uqa_planner::{QueryPlan, RelationalPlan, SourcePlan};

type AnalyzeValues = BTreeMap<String, Vec<Value>>;
type AnalyzeNullCounts = BTreeMap<String, u64>;

mod analyze;
mod analyze_helpers;
mod schemas;
mod settings;
mod settings_parse;
mod view_binding;
mod views;

use analyze_helpers::collect_analyze_values;
use settings_parse::parse_search_path_list;
use view_binding::{
    bind_query_plan_relations, bind_query_plan_sequence_references,
    canonical_virtual_relation_reference, query_plan_references_relation,
    query_plan_references_sequence,
};

#[cfg(test)]
use view_binding::sequence_function_reference_mut;

#[cfg(test)]
mod tests;
