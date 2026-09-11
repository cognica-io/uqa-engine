//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent and in-memory backing stores for UQA: documents, inverted
//! index, vector indexes (IVF), B-tree, in-memory spatial scan, block-max, and the
//! provider-neutral catalog and transaction contracts.

pub mod backend;
pub mod block_max_index;
pub mod btree_index;
pub mod catalog_index_keys;
mod value_index_key;
pub use value_index_key::ValueIndexKey;
pub mod catalog;
pub mod clustered_postings;
pub mod document_store;
pub mod hnsw_index;
pub mod index_abc;
pub mod index_manager;
pub mod index_types;
pub mod inverted_index;
pub mod ivf_index;
pub mod key_value;
pub mod spatial_index;
pub mod transaction;
pub mod vector_index;

pub use backend::{
    PersistentStorageBackend, PersistentStorageIdentity, PersistentStorageProvider,
    PersistentStorageSession, StorageBackendError, StorageBackendResult, StorageSavepointId,
};
pub use block_max_index::{BlockMaxIndex, BlockMaxScorer, DEFAULT_BLOCK_SIZE};
pub use btree_index::BTreeIndex;
pub use catalog::{
    sequence_value_reservation, CatalogCacheRevisions, CatalogFacade, CatalogIndexRow,
    ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow, GraphEntityFilter, GraphEntityKind,
    GraphSnapshot, GraphVertexRow, RelationIdentity, RelationKind, SchemaAclEntry,
    SchemaPrivileges, SchemaRow, SequenceAclEntry, SequenceOptions, SequenceOwner,
    SequenceOwnerDependency, SequencePrivileges, SequenceReservationResult, SequenceRow,
    SequenceValuePosition, SequenceValueReservation, TableAclEntry, TablePrivileges, TableSchema,
    VectorFieldSchema, ViewRow, MAX_GRAPH_ID_PAGE,
};
pub use clustered_postings::{
    MaterializedPostingCursor, PostingCursor, PostingScore, POSTING_CLUSTER_DOCS,
};
pub use document_store::{
    DocumentMetadata, DocumentStore, MemoryDocumentStore, SharedDocumentRow, StoredDocument,
};
pub use hnsw_index::HNSWIndex;
pub use index_abc::Index;
pub use index_manager::{BTreeIndexHandle, IndexManager};
pub use index_types::{IndexDef, IndexType};
pub use inverted_index::{AnalyzerPhase, InvertedIndex, MemoryInvertedIndex};
pub use ivf_index::{IVFIndex, IVFState};
pub use key_value::{
    KeyValueBatch, KeyValueCatalog, KeyValueDocumentStore, KeyValueInvertedIndex,
    KeyValueStorageBackend, KeyValueStore, KeyValueVectorIndex, MemoryKeyValueStore,
};
pub use spatial_index::{haversine_distance, MemorySpatialIndex, SpatialIndex};
pub use transaction::{InMemoryTransaction, Snapshotable, TransactionError, TxResult};
pub use vector_index::{
    cosine_similarity, HNSWIndexParams, IVFIndexParams, MemoryVectorIndex, VectorIndex,
    VectorIndexOpenMode, VectorIndexSpec,
};

mod fts_index_stat;
pub use fts_index_stat::FtsIndexStat;
