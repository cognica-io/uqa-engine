//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture fixed physical facts without constructing an intermediate JSON object tree.

use crate::cost_model::DiskANNWorkEstimate;
use std::fmt::Write as _;
use uqa_core::{
    memory::{ProductionControl, ProductionString},
    DiskANNIndexStats,
};
use uqa_sql::{
    result::explain::{ExplainObject, ExplainValue},
    SQLError,
};

pub(super) fn index_node<'a>(
    stats: &DiskANNIndexStats,
    control: ProductionControl<'a>,
) -> Result<ExplainObject<'a>, SQLError> {
    let mut node = ExplainObject::new(control);
    let mut generation = ExplainObject::new(control);
    let mut database = ProductionString::new(control);
    for byte in stats.generation.database {
        let _ = write!(database, "{byte:02x}");
    }
    generation.insert("Database", ExplainValue::Text(database.finish()?))?;
    generation.number("Table", stats.generation.table)?;
    generation.number("Index", stats.generation.index)?;
    generation.number("Generation", stats.generation.generation)?;
    node.insert("Generation", generation.finish()?)?;
    let mut configuration = ExplainObject::new(control);
    configuration.number("Dimensions", stats.dimensions.into())?;
    configuration.number("Maximum Degree", stats.max_degree as u64)?;
    configuration.number("Search List Size", stats.search_list_size as u64)?;
    configuration.number("Logical Beam Width", stats.beam_width as u64)?;
    configuration.number("PQ Bytes", stats.pq_bytes as u64)?;
    configuration.optional_number("PQ Centroids", stats.pq_centroids.map(u64::from))?;
    node.insert("Configuration", configuration.finish()?)?;
    let mut layout = ExplainObject::new(control);
    layout.number("Node Slot Bytes", stats.node_slot_bytes as u64)?;
    layout.number("Fragments Per Node", stats.node_fragments.into())?;
    layout.number("Graph Pages", stats.graph_pages)?;
    layout.optional_number("Graph Edges", stats.graph_edges)?;
    layout.number("Page Bytes", stats.page_bytes as u64)?;
    node.insert("Layout", layout.finish()?)?;
    let population = stats.populations;
    let mut populations = ExplainObject::new(control);
    populations.optional_number("Base Documents", population.base_documents)?;
    populations.number("Base Vectors", population.base_vectors)?;
    populations.number("Graph Nodes", population.graph_nodes)?;
    populations.number("Side Vectors", population.side_vectors)?;
    populations.optional_number("Current Vectors", population.current_vectors)?;
    populations.optional_number("Changed Vectors", population.changed_vectors)?;
    node.insert("Population", populations.finish()?)?;
    let reads = stats.reads;
    let mut limits = ExplainObject::new(control);
    limits.number("Resident Bytes", reads.resident_bytes as u64)?;
    limits.number("Cache Bytes", reads.cache_bytes as u64)?;
    limits.number("Maximum Record Bytes", reads.max_record_bytes as u64)?;
    limits.number(
        "Maximum In-Flight Page Bytes",
        reads.max_in_flight_page_bytes as u64,
    )?;
    limits.number("Maximum Batch Pages", reads.max_batch_pages as u64)?;
    limits.number("Read Concurrency", reads.read_concurrency as u64)?;
    node.insert("Read Limits", limits.finish()?)?;
    Ok(node)
}

pub(super) fn work_node(
    work: DiskANNWorkEstimate,
    control: ProductionControl<'_>,
) -> Result<ExplainValue, SQLError> {
    let mut node = ExplainObject::new(control);
    node.label(
        "Units",
        "uncalibrated work; logical page requests, not measured physical I/O",
    )?;
    node.float("Approximate Nodes", work.approximate_nodes)?;
    node.float("Completion Nodes", work.completion_nodes)?;
    node.float("PQ Lookup Coordinates", work.pq_lookup_coordinates)?;
    node.float("PQ Distance Evaluations", work.pq_distance_evaluations)?;
    node.float("Logical Page Requests", work.logical_page_requests)?;
    node.float("Logical Page Bytes", work.logical_page_bytes)?;
    node.float("Provider Dispatch Rounds", work.page_rounds)?;
    node.float("Side Vectors", work.side_vectors)?;
    node.float("Changed Vectors", work.changed_vectors)?;
    node.float("Rerank Vectors", work.rerank_vectors)?;
    node.float("Exact Vectors", work.exact_vectors)?;
    node.float("Resident PQ Payload Bytes", work.resident_pq_payload_bytes)?;
    node.boolean("Page Budget Sufficient", work.page_budget_sufficient)?;
    node.float("CPU", work.cpu)?;
    node.float("I/O", work.io)?;
    node.float("Total", work.total())?;
    node.finish()
}
