//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native physical family IDs are durable format assignments. Append new IDs; never reorder or reuse an existing assignment.

use super::layout::{NativeRecordLayout, LAYOUTS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum NativeRecordFamily {
    Analyzers = 1,
    BtreeIndexEntries = 2,
    BtreeIndexRepairs = 3,
    BtreeIndexes = 4,
    CacheRevisions = 5,
    CatalogIndexes = 6,
    ColumnStats = 7,
    DocLengths = 8,
    DocumentBlobs = 9,
    Documents = 10,
    FieldStats = 11,
    ForeignServers = 12,
    ForeignTables = 13,
    GraphEdges = 14,
    GraphMembership = 15,
    GraphPathIndexState = 16,
    GraphPathPairs = 17,
    GraphVertices = 18,
    HNSWEdges = 19,
    HNSWIndexes = 20,
    HNSWNodes = 21,
    IVFAssignments = 22,
    IVFCentroids = 23,
    IVFIndexes = 24,
    Metadata = 25,
    Models = 26,
    NamedGraphs = 27,
    OccurrenceClusters = 28,
    OccurrenceDocuments = 29,
    OccurrenceFields = 30,
    OccurrenceFormats = 31,
    OccurrenceLengths = 32,
    PathIndexes = 33,
    PostingClusters = 34,
    PostingDocuments = 35,
    Relations = 36,
    Schemas = 37,
    ScoringParams = 38,
    Sequences = 39,
    TableFieldAnalyzers = 40,
    Tables = 41,
    Vectors = 42,
    Views = 43,
    TableOwners = 44,
    GraphLookups = 45,
    OccurrenceSkips = 46,
    OccurrenceBlockMax = 47,
    OccurrenceGuards = 48,
    VectorGuards = 49,
    StandaloneGraphScopes = 50,
    StandaloneGraphMetadata = 51,
    StandaloneGraphCatalog = 52,
    StandaloneGraphVertices = 53,
    StandaloneGraphEdges = 54,
    StandaloneGraphMembership = 55,
    StandaloneGraphLookups = 56,
    DiskANNRecords = 57,
    VectorOrigins = 58,
    VectorChanges = 59,
}

impl NativeRecordFamily {
    pub const fn id(self) -> u16 {
        self as u16
    }

    pub fn from_id(id: u16) -> Option<Self> {
        if id == Self::VectorChanges.id() {
            return Some(Self::VectorChanges);
        }
        if id == Self::VectorOrigins.id() {
            return Some(Self::VectorOrigins);
        }
        if id == Self::DiskANNRecords.id() {
            return Some(Self::DiskANNRecords);
        }
        usize::from(id)
            .checked_sub(1)
            .and_then(|index| {
                LAYOUTS.get(index).or_else(|| {
                    index
                        .checked_sub(LAYOUTS.len())
                        .and_then(|index| super::standalone_graph::schema::LAYOUTS.get(index))
                })
            })
            .map(|layout| layout.family)
    }

    pub fn layout(self) -> &'static NativeRecordLayout {
        if self == Self::VectorChanges {
            return &super::diskann::CHANGES_LAYOUT;
        }
        if self == Self::VectorOrigins {
            return &super::diskann::ORIGINS_LAYOUT;
        }
        if self == Self::DiskANNRecords {
            return &super::diskann::LAYOUT;
        }
        let index = usize::from(self.id()) - 1;
        if index < LAYOUTS.len() {
            &LAYOUTS[index]
        } else {
            &super::standalone_graph::schema::LAYOUTS[index - LAYOUTS.len()]
        }
    }

    pub(crate) fn is_standalone_graph(self) -> bool {
        (Self::StandaloneGraphScopes.id()..=Self::StandaloneGraphLookups.id()).contains(&self.id())
    }

    pub fn all() -> impl ExactSizeIterator<Item = Self> {
        (1..=Self::VectorChanges.id()).map(|id| Self::from_id(id).expect("assigned native family"))
    }
}
