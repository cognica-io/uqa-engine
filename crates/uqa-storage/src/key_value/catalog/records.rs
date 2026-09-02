//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serialized key/value catalog records shared by migration and facade operations.

use super::{Deserialize, RelationKind, Serialize};

pub(super) const LEGACY_VIEWS_METADATA_KEY: &str = "sql_views_json";
pub(super) const LEGACY_SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredVertex {
    pub(super) label: String,
    pub(super) properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredEdge {
    pub(super) source_id: u64,
    pub(super) target_id: u64,
    pub(super) label: String,
    pub(super) properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredForeignServer {
    pub(super) fdw_type: String,
    pub(super) options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredForeignTable {
    pub(super) role_owner: String,
    pub(super) server_name: String,
    pub(super) columns_json: String,
    pub(super) options_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyStoredForeignTable {
    pub(super) server_name: String,
    pub(super) columns_json: String,
    pub(super) options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredRelation {
    pub(super) kind: RelationKind,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredView {
    pub(super) role_owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) acl: Option<Vec<crate::catalog::TableAclEntry>>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(super) column_acls: std::collections::BTreeMap<String, Vec<crate::catalog::TableAclEntry>>,
    pub(super) definition_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyStoredView {
    pub(super) definition_json: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct LegacyTableSchema {
    pub(super) name: String,
    pub(super) analyzer_json: String,
    pub(super) fts_fields: Vec<String>,
    pub(super) vector_fields: Vec<crate::catalog::VectorFieldSchema>,
    #[serde(default)]
    pub(super) columns_json: String,
    #[serde(default)]
    pub(super) constraints_json: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct LegacySequenceState {
    pub(super) start: i64,
    pub(super) increment: i64,
    pub(super) current: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredCatalogIndex {
    pub(super) index_type: String,
    pub(super) table_name: String,
    pub(super) columns_json: String,
    pub(super) parameters_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredColumnStats {
    pub(super) distinct_count: i64,
    pub(super) null_count: i64,
    pub(super) min_value: Option<String>,
    pub(super) max_value: Option<String>,
    pub(super) row_count: i64,
    pub(super) histogram_json: String,
    pub(super) mcv_values_json: String,
    pub(super) mcv_frequencies_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredSequence {
    #[serde(default = "legacy_sequence_role_owner")]
    pub(super) role_owner: String,
    #[serde(default)]
    pub(super) acl: Option<Vec<crate::catalog::SequenceAclEntry>>,
    #[serde(default)]
    pub(super) object_id: [u8; 16],
    #[serde(default)]
    pub(super) definition_generation: [u8; 16],
    pub(super) start: i64,
    pub(super) increment: i64,
    pub(super) current: i64,
    #[serde(default = "legacy_sequence_called")]
    pub(super) called: bool,
    #[serde(default)]
    pub(super) log_count: i64,
    #[serde(default = "legacy_sequence_persistence")]
    pub(super) persistence: String,
    #[serde(default)]
    pub(super) owner: Option<crate::catalog::SequenceOwner>,
    #[serde(default)]
    pub(super) options: crate::catalog::SequenceOptions,
}

pub(super) const fn legacy_sequence_called() -> bool {
    true
}

pub(super) fn legacy_sequence_persistence() -> String {
    "p".into()
}

pub(super) fn legacy_sequence_role_owner() -> String {
    "uqa".into()
}
