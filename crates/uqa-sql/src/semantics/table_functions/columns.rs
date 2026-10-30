//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Empty table-function descriptors produce final names without copying overwritten defaults or aliases.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub fn table_function_empty_schema_with_control(
    name: &str,
    output_name: &str,
    alias: Option<&str>,
    aliases: &[String],
    output_width: usize,
    ordinality: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let lower = crate::semantics::builtin_function_dispatch_name_with_control(name, control)?;
    let defaults: &[&str] = match lower.as_str() {
        "json_each" | "jsonb_each" | "json_each_text" | "jsonb_each_text" => &["key", "value"],
        "pg_get_sequence_data" => &["last_value", "is_called"],
        "aclexplode" => &["grantor", "grantee", "privilege_type", "is_grantable"],
        "pg_sequence_parameters" => &[
            "start_value",
            "minimum_value",
            "maximum_value",
            "increment",
            "cycle_option",
            "cache_size",
            "data_type",
        ],
        "pagerank" | "graph_pagerank" | "hits" | "graph_hits" | "betweenness"
        | "graph_betweenness" => &["_doc_id", "_score"],
        "rpq" => &["vertex_id"],
        "list_analyzers" => &["analyzer_name"],
        "analyze_text" => &["analysis"],
        "fts_index_stats" => &[
            "table_name",
            "field",
            "analyzer",
            "posting_count",
            "doc_length_count",
            "indexed_doc_count",
            "term_count",
            "total_field_length",
        ],
        "text_similarity_join"
        | "vector_similarity_join"
        | "graph_join"
        | "hybrid_join"
        | "cross_paradigm_join" => &["left_doc_id", "right_doc_id", "_score"],
        _ => &[],
    };
    let width = if !defaults.is_empty() {
        defaults.len()
    } else if lower.as_str() == "unnest" {
        output_width.max(1)
    } else if matches!(
        lower.as_str(),
        "generate_series"
            | "pg_listening_channels"
            | "regexp_matches"
            | "regexp_split_to_table"
            | "string_to_table"
            | "json_array_elements"
            | "jsonb_array_elements"
            | "json_array_elements_text"
            | "jsonb_array_elements_text"
            | "json_object_keys"
            | "jsonb_object_keys"
    ) {
        1
    } else {
        if ordinality && aliases.len() > 1 {
            aliases.len() - 1
        } else {
            aliases.len()
        }
        .max(1)
    };
    let default = if super::is_json_array_table_function(&lower) {
        "value"
    } else if lower.as_str() == "unnest" && width > 1 {
        output_name
    } else {
        alias.unwrap_or(output_name)
    };
    let mut columns = ProductionVec::new(*control);
    columns.reserve(width + usize::from(ordinality))?;
    for position in 0..width + usize::from(ordinality) {
        let name = aliases.get(position).map_or_else(
            || {
                if position == width {
                    "ordinality"
                } else {
                    defaults.get(position).copied().unwrap_or(default)
                }
            },
            String::as_str,
        );
        columns.push_produced(control.copy_text(name)?)?;
    }
    Ok(columns.finish()?)
}
