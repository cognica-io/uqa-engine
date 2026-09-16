//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Evaluated graph changes and provider record addressing for derived path state.

mod resolve;

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::{read_control::StorageReadControl, GraphEntityKind};

use super::{CommitSequence, DatabaseId, MergedRecordSnapshot, VersionResult};

/// A logical change whose derived path-cache effects must use current commit dependencies. These operations never execute graph algorithms or application callbacks.
#[derive(Clone, Copy)]
pub enum GraphMutation<'a> {
    InvalidateGraph(&'a str),
    InvalidateEntity(GraphEntityKind, u64),
    PublishPath {
        index: &'a str,
        graph: &'a str,
        definition: &'a str,
    },
}

/// Logical addresses and scan prefixes; each provider owns its durable byte encoding.
#[derive(Clone, Copy)]
pub enum GraphRecordKey<'a> {
    Entity(GraphEntityKind, u64),
    EntityMemberships(GraphEntityKind, u64),
    GraphMemberships(&'a str),
    GraphPaths(&'a str),
    GraphName(&'a str),
    LabelRegistry(&'a str),
    PathDefinition(&'a str),
    PathValidity(&'a str),
}

/// Physical addressing and row codecs only. Common MVCC owns dependency discovery, invalidation order, build validation and commit retries.
pub trait GraphRecordLayout: Send + Sync {
    fn key(
        &self,
        database: DatabaseId,
        key: GraphRecordKey<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn is_validity_key(&self, key: &[u8]) -> VersionResult<bool>;

    fn membership_graph(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>>;

    fn membership_entity(&self, key: &[u8]) -> VersionResult<(GraphEntityKind, u64)>;

    /// Decode one path-directory entry; return its validity key only when it belongs to the selected graph.
    fn path_validity_key(
        &self,
        view: &MergedRecordSnapshot,
        database: DatabaseId,
        graph: &str,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>>;

    fn invalidate(
        &self,
        key: &[u8],
        value: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<BudgetedVec<u8>>>;

    /// Return whether the transaction's final private state still contains this completed build, rather than a later clear or invalidation.
    fn is_published(
        &self,
        view: &MergedRecordSnapshot,
        key: &[u8],
        value: &[u8],
        graph: &str,
        definition: &str,
        control: &StorageReadControl,
    ) -> VersionResult<bool>;

    fn definition_matches(
        &self,
        key: &[u8],
        value: &[u8],
        definition: &str,
        control: &StorageReadControl,
    ) -> VersionResult<bool>;
}

type Text = Arc<BudgetedVec<u8>>;

#[derive(Clone)]
pub(super) enum OwnedGraphMutation {
    InvalidateGraph(Text),
    InvalidateEntity(GraphEntityKind, u64),
    PublishPath {
        index: Text,
        graph: Text,
        definition: Text,
    },
}

impl OwnedGraphMutation {
    pub(super) fn retain(
        value: GraphMutation<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let text = |value: &str| {
            let mut bytes = BudgetedVec::new(control.memory());
            bytes.extend_from_slice(value.as_bytes())?;
            Ok::<_, super::VersionError>(Arc::new(bytes))
        };
        Ok(match value {
            GraphMutation::InvalidateGraph(graph) => Self::InvalidateGraph(text(graph)?),
            GraphMutation::InvalidateEntity(kind, id) => Self::InvalidateEntity(kind, id),
            GraphMutation::PublishPath {
                index,
                graph,
                definition,
            } => Self::PublishPath {
                index: text(index)?,
                graph: text(graph)?,
                definition: text(definition)?,
            },
        })
    }

    pub(super) fn borrowed(&self) -> GraphMutation<'_> {
        fn text(bytes: &Text) -> &str {
            std::str::from_utf8(bytes).expect("retained graph text")
        }
        match self {
            Self::InvalidateGraph(graph) => GraphMutation::InvalidateGraph(text(graph)),
            Self::InvalidateEntity(kind, id) => GraphMutation::InvalidateEntity(*kind, *id),
            Self::PublishPath {
                index,
                graph,
                definition,
            } => GraphMutation::PublishPath {
                index: text(index),
                graph: text(graph),
                definition: text(definition),
            },
        }
    }
}

pub(super) struct GraphEffects {
    pub(super) base: CommitSequence,
    pub(super) operations: BudgetedVec<OwnedGraphMutation>,
}

pub(super) use resolve::resolve;
