//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AGE graphid composition and per-graph label allocation state.

use super::{BTreeMap, Deserialize, GraphStoreError, GraphStoreResult, Serialize};

/// Number of bits reserved for the per-label sequence inside an AGE
/// `graphid`. The label id occupies the remaining high 16 bits.
pub const GRAPHID_LABEL_SHIFT: u32 = 48;

/// Reserved AGE label id for unlabeled vertices (`_ag_label_vertex`).
pub const VERTEX_DEFAULT_LABEL_ID: u32 = 1;

/// Reserved AGE label id for unlabeled edges (`_ag_label_edge`).
pub const EDGE_DEFAULT_LABEL_ID: u32 = 2;

/// First label id available to user labels.
pub const FIRST_USER_LABEL_ID: u32 = 3;

/// The largest label id whose AGE graphid remains representable as a signed
/// 64-bit agtype integer.
pub const MAX_GRAPHID_LABEL_ID: u32 = 32_767;

pub(super) const MAX_GRAPHID_SEQUENCE: u64 = (1_u64 << GRAPHID_LABEL_SHIFT) - 1;
const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

pub(super) fn usize_to_f64_exact(value: usize, context: &str) -> GraphStoreResult<f64> {
    if u64::try_from(value).is_ok_and(|value| value <= MAX_EXACT_F64_INTEGER) {
        Ok(value as f64)
    } else {
        Err(GraphStoreError::InvalidMutation(format!(
            "{context} {value} exceeds the exact f64 integer range"
        )))
    }
}

/// Compose an AGE `graphid` from a label id and per-label sequence.
pub fn make_graphid(label_id: u32, sequence: u64) -> GraphStoreResult<u64> {
    if label_id > MAX_GRAPHID_LABEL_ID {
        return Err(GraphStoreError::IdExhausted(format!(
            "label id {label_id} exceeds {MAX_GRAPHID_LABEL_ID}"
        )));
    }
    if sequence == 0 || sequence > MAX_GRAPHID_SEQUENCE {
        return Err(GraphStoreError::IdExhausted(format!(
            "sequence {sequence} is outside 1..={MAX_GRAPHID_SEQUENCE}"
        )));
    }
    Ok((u64::from(label_id) << GRAPHID_LABEL_SHIFT) | sequence)
}

/// Label id component of an AGE `graphid`.
#[must_use]
pub fn graphid_label_id(id: u64) -> u32 {
    let bytes = id.to_be_bytes();
    u32::from(u16::from_be_bytes([bytes[0], bytes[1]]))
}

/// Sequence component of an AGE `graphid`.
#[must_use]
pub fn graphid_sequence(id: u64) -> u64 {
    id & ((1 << GRAPHID_LABEL_SHIFT) - 1)
}

/// Per-graph AGE label registry: label name -> label id plus the
/// per-label id sequences. Serializable so engines can persist it in
/// catalog metadata and restore deterministic id allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphLabelRegistry {
    /// Label name -> AGE label id. Vertex and edge labels share the
    /// namespace-wide counter; the reserved names for ids 1 / 2 are
    /// not stored here (empty labels map onto them implicitly).
    pub labels: BTreeMap<String, u32>,
    /// Label id -> last allocated per-label sequence value.
    pub sequences: BTreeMap<u32, u64>,
    /// Next label id handed to a previously unseen label.
    pub next_label_id: u32,
}

impl Default for GraphLabelRegistry {
    fn default() -> Self {
        Self {
            labels: BTreeMap::new(),
            sequences: BTreeMap::new(),
            next_label_id: FIRST_USER_LABEL_ID,
        }
    }
}

impl GraphLabelRegistry {
    pub(super) fn label_id(&mut self, label: &str, default_id: u32) -> GraphStoreResult<u32> {
        if label.is_empty() {
            return Ok(default_id);
        }
        if let Some(id) = self.labels.get(label) {
            if *id > MAX_GRAPHID_LABEL_ID {
                return Err(GraphStoreError::IdExhausted(format!(
                    "persisted label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
                )));
            }
            return Ok(*id);
        }
        let id = self.next_label_id;
        if id > MAX_GRAPHID_LABEL_ID {
            return Err(GraphStoreError::IdExhausted(format!(
                "label id {id} exceeds {MAX_GRAPHID_LABEL_ID}"
            )));
        }
        self.next_label_id = id
            .checked_add(1)
            .ok_or_else(|| GraphStoreError::IdExhausted("label id counter overflow".to_string()))?;
        self.labels.insert(label.to_string(), id);
        Ok(id)
    }

    pub(super) fn next_sequence(&mut self, label_id: u32) -> GraphStoreResult<u64> {
        let current = self.sequences.get(&label_id).copied().unwrap_or(0);
        let next = current.checked_add(1).ok_or_else(|| {
            GraphStoreError::IdExhausted(format!(
                "sequence counter overflow for label id {label_id}"
            ))
        })?;
        if next > MAX_GRAPHID_SEQUENCE {
            return Err(GraphStoreError::IdExhausted(format!(
                "sequence {next} exceeds {MAX_GRAPHID_SEQUENCE} for label id {label_id}"
            )));
        }
        self.sequences.insert(label_id, next);
        Ok(next)
    }

    /// Fold an existing entity id back into the registry so restored
    /// graphs never re-issue an id that is already in use.
    pub(super) fn observe(&mut self, label: &str, id: u64) {
        let label_id = graphid_label_id(id);
        if label_id == 0 {
            // Pre-AGE id (plain counter) - nothing to learn.
            return;
        }
        if !label.is_empty() && label_id >= FIRST_USER_LABEL_ID {
            self.labels.entry(label.to_string()).or_insert(label_id);
        }
        let seq = graphid_sequence(id);
        let entry = self.sequences.entry(label_id).or_insert(0);
        if seq > *entry {
            *entry = seq;
        }
        if label_id >= self.next_label_id {
            self.next_label_id = label_id + 1;
        }
    }

    /// Merge another registry (e.g. persisted metadata) into this one,
    /// keeping the larger sequence values and label id watermark.
    pub fn merge(&mut self, other: &GraphLabelRegistry) {
        for (label, id) in &other.labels {
            self.labels.entry(label.clone()).or_insert(*id);
        }
        for (label_id, seq) in &other.sequences {
            let entry = self.sequences.entry(*label_id).or_insert(0);
            if *seq > *entry {
                *entry = *seq;
            }
        }
        if other.next_label_id > self.next_label_id {
            self.next_label_id = other.next_label_id;
        }
    }
}
