//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! AGE graphid composition and per-graph label allocation state.

use super::{BTreeMap, BTreeSet, Deserialize, GraphStoreError, GraphStoreResult, Serialize};

use crate::age_names::{EDGE_DEFAULT_LABEL_NAME, VERTEX_DEFAULT_LABEL_NAME};

pub(super) mod registry;

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

pub(crate) const MAX_GRAPHID_SEQUENCE: u64 = (1_u64 << GRAPHID_LABEL_SHIFT) - 1;
const MAX_EXACT_F64_INTEGER: u64 = 9_007_199_254_740_992;

pub(crate) fn usize_to_f64_exact(value: usize, context: &str) -> GraphStoreResult<f64> {
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

/// AGE label kind: the `ag_label.kind` catalog value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LabelKind {
    /// A vertex label (`ag_label.kind = 'v'`).
    #[serde(rename = "v")]
    Vertex,
    /// An edge label (`ag_label.kind = 'e'`).
    #[serde(rename = "e")]
    Edge,
}

impl LabelKind {
    /// The `ag_label.kind` character.
    #[must_use]
    pub fn as_char(self) -> char {
        match self {
            Self::Vertex => 'v',
            Self::Edge => 'e',
        }
    }

    /// The reserved label id used for unlabeled entities of this kind.
    #[must_use]
    pub fn default_label_id(self) -> u32 {
        match self {
            Self::Vertex => VERTEX_DEFAULT_LABEL_ID,
            Self::Edge => EDGE_DEFAULT_LABEL_ID,
        }
    }

    /// The reserved AGE default label name for this kind.
    #[must_use]
    pub fn default_label_name(self) -> &'static str {
        match self {
            Self::Vertex => VERTEX_DEFAULT_LABEL_NAME,
            Self::Edge => EDGE_DEFAULT_LABEL_NAME,
        }
    }

    fn entity_noun(self) -> &'static str {
        match self {
            Self::Vertex => "vertices",
            Self::Edge => "edges",
        }
    }
}

/// One `ag_label` catalog entry of a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphLabelInfo {
    /// Label name; the default labels use the reserved AGE names.
    pub name: String,
    /// AGE label id (the high 16 bits of every graphid under the label).
    pub id: u32,
    /// Vertex or edge label.
    pub kind: LabelKind,
    /// Last allocated per-label sequence value (0 when nothing was
    /// allocated yet).
    pub last_sequence: u64,
}

/// Per-graph AGE label registry: label name -> label id plus the
/// per-label id sequences. Serializable so engines can persist it in
/// catalog metadata and restore deterministic id allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphLabelRegistry {
    /// Persistent allocation identity, preserved by rename. Zero denotes a legacy registry or an in-memory graph.
    pub allocation_id: [u8; 16],
    /// Label name -> AGE label id. Vertex and edge labels share the
    /// namespace-wide counter; the reserved names for ids 1 / 2 are
    /// not stored here (empty labels map onto them implicitly).
    pub labels: BTreeMap<String, u32>,
    /// Label name -> vertex or edge kind. Registries persisted before
    /// kinds were recorded fill this map from the stored entities.
    pub kinds: BTreeMap<String, LabelKind>,
    /// Label id -> last allocated per-label sequence value.
    pub sequences: BTreeMap<u32, u64>,
    /// AGE label ids whose relations were removed through `drop_label`.
    /// Tombstones are persisted because edge rows can outlive the vertex
    /// label relations that owned their endpoints. Registries written before
    /// this field existed deserialize as an empty set.
    pub dropped_label_ids: BTreeSet<u32>,
    /// Next label id handed to a previously unseen label.
    pub next_label_id: u32,
}

impl Default for GraphLabelRegistry {
    fn default() -> Self {
        Self {
            allocation_id: [0; 16],
            labels: BTreeMap::new(),
            kinds: BTreeMap::new(),
            sequences: BTreeMap::new(),
            dropped_label_ids: BTreeSet::new(),
            next_label_id: FIRST_USER_LABEL_ID,
        }
    }
}

impl GraphLabelRegistry {
    /// Resolve the label id for an entity of `kind`, allocating a new
    /// user label on first use. Empty labels map onto the reserved
    /// default label of the kind. Using a label registered for the
    /// other kind fails exactly like AGE's `CREATE` transform.
    pub(crate) fn label_id(&mut self, label: &str, kind: LabelKind) -> GraphStoreResult<u32> {
        registry::label_id(self, label, kind)
    }

    /// Whether `label` names a registered user label or a reserved
    /// default label.
    #[must_use]
    pub fn contains_label(&self, label: &str) -> bool {
        registry::contains_label(self, label)
    }

    /// The kind of a registered or default label. A user label persisted
    /// before kinds were recorded, and whose entities are all gone, reports
    /// as a vertex label until its next use records the kind.
    #[must_use]
    pub fn label_kind(&self, label: &str) -> Option<LabelKind> {
        registry::label_kind(self, label)
    }

    /// Register an empty user label ahead of any entity, as
    /// `create_vlabel` / `create_elabel` do. Returns the new label id;
    /// `None` when the name is already a label of this graph.
    pub fn register_label(
        &mut self,
        label: &str,
        kind: LabelKind,
    ) -> GraphStoreResult<Option<u32>> {
        registry::register_label(self, label, kind)
    }

    /// Forget a label. Default labels leave a durable tombstone so the graph
    /// can continue to exist without their AGE relations. Returns the
    /// released label id, or `None` when the label is not registered.
    pub fn remove_label(&mut self, label: &str) -> Option<u32> {
        match registry::remove_label(self, label) {
            Ok(value) => value,
            Err(error) => match error {},
        }
    }

    /// Every present label of the graph in `ag_label` order: surviving
    /// defaults first, then user labels by ascending label id.
    #[must_use]
    pub fn labels(&self) -> Vec<GraphLabelInfo> {
        registry::labels(self)
    }

    pub(crate) fn next_sequence(&mut self, label_id: u32) -> GraphStoreResult<u64> {
        registry::next_sequence(self, label_id)
    }

    /// Fold an existing entity id into the allocation watermarks and report whether the registry changed.
    pub(crate) fn observe(&mut self, label: &str, id: u64, kind: LabelKind) -> bool {
        match registry::observe(self, label, id, kind) {
            Ok(value) => value,
            Err(error) => match error {},
        }
    }

    /// Merge another registry (e.g. persisted metadata) into this one,
    /// keeping the larger sequence values and label id watermark.
    pub fn merge(&mut self, other: &GraphLabelRegistry) {
        match registry::merge(self, other) {
            Ok(()) => (),
            Err(error) => match error {},
        }
    }
}
