//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Serialized key/value catalog records shared by migration and facade operations.

use super::{Deserialize, RelationKind, Serialize};

pub(super) const LEGACY_VIEWS_METADATA_KEY: &str = "sql_views_json";
pub(super) const LEGACY_SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";
pub(super) const STORED_FOREIGN_TABLE_SECURITY_VERSION: u8 = 1;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) metadata_json: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredForeignTable {
    pub(super) security_version: u8,
    #[serde(flatten)]
    pub(super) security: crate::RelationSecurityRow,
    pub(super) server_name: String,
    pub(super) columns_json: String,
    pub(super) options_json: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OwnedStoredForeignTable {
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
    #[serde(flatten)]
    pub(super) security: crate::RelationSecurityRow,
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
    #[serde(default)]
    pub(super) definition_json: Option<String>,
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
    #[serde(flatten)]
    pub(super) security: crate::catalog::SequenceSecurityRow,
    #[serde(default)]
    pub(super) object_id: [u8; 16],
    #[serde(default)]
    pub(super) definition_generation: [u8; 16],
    pub(super) start: i64,
    pub(super) increment: i64,
    /// Value state that earlier releases kept in the definition. A current definition keeps it in the value record of its generation and omits these fields, which earlier releases require, so that they cannot allocate from a definition whose value state has moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) current: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) called: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) log_count: Option<i64>,
    #[serde(default = "legacy_sequence_persistence")]
    pub(super) persistence: String,
    #[serde(default)]
    pub(super) owner: Option<crate::catalog::SequenceOwner>,
    #[serde(default)]
    pub(super) options: crate::catalog::SequenceOptions,
}

impl StoredSequence {
    /// The value state of a definition written by an earlier release, which kept it there.
    pub(super) fn legacy_value(&self) -> Option<crate::catalog::SequenceValuePosition> {
        self.current
            .map(|current| crate::catalog::SequenceValuePosition {
                current,
                // Definitions older than the called flag were always called.
                called: self.called.unwrap_or(true),
                log_count: self.log_count.unwrap_or(0),
            })
    }
}

/// The value state of one definition generation of a sequence, with the allocation options the generation was created with, which never change. Its key names the sequence's object identity and the generation, not the sequence's name, so that a value operation outside the transaction that renames the sequence finds it.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredSequenceValue {
    pub(super) current: i64,
    pub(super) called: bool,
    pub(super) log_count: i64,
    pub(super) increment: i64,
    pub(super) min_value: i64,
    pub(super) max_value: i64,
    pub(super) cycle: bool,
    pub(super) cache_size: i64,
}

impl StoredSequenceValue {
    pub(super) const fn position(&self) -> crate::catalog::SequenceValuePosition {
        crate::catalog::SequenceValuePosition {
            current: self.current,
            called: self.called,
            log_count: self.log_count,
        }
    }
}

pub(super) fn legacy_sequence_persistence() -> String {
    "p".into()
}
