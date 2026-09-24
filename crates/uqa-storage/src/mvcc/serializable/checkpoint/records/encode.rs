//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Individually checksummed records stream borrowed predicates directly into provider values.

use std::io::Write;

use sha2::{Digest, Sha256};

use super::super::{io::Encoder, nodes, predicates, CountBytes};
use super::{SerializableCheckpointKey, SerializableGraph, VersionResult, MAGIC};
use crate::{
    mvcc::{
        serializable::{observations::Observation, Edge, Transaction},
        DatabaseId,
    },
    read_control::StorageReadControl,
};

enum Body<'a> {
    Header(&'a SerializableGraph),
    Transaction(Transaction),
    Edge(Edge),
    Predicate(&'a Observation, bool),
}

/// One borrowed checkpoint record to insert atomically with the rest of an admitted change stream. It includes its database/coordinator identity and checksum. Providers own physical allocation and durability, not record interpretation.
pub struct SerializableCheckpointRecord<'a> {
    database: DatabaseId,
    coordinator: [u8; 16],
    body: Body<'a>,
}

impl<'a> SerializableCheckpointRecord<'a> {
    fn new(graph: &SerializableGraph, body: Body<'a>) -> Self {
        Self {
            database: graph.database,
            coordinator: graph.coordinator,
            body,
        }
    }

    pub(super) fn header(graph: &'a SerializableGraph) -> Self {
        Self::new(graph, Body::Header(graph))
    }

    pub(super) fn transaction(graph: &SerializableGraph, entry: Transaction) -> Self {
        Self::new(graph, Body::Transaction(entry))
    }

    pub(super) fn edge(graph: &SerializableGraph, edge: Edge) -> Self {
        Self::new(graph, Body::Edge(edge))
    }

    pub(in crate::mvcc::serializable) fn predicate(
        graph: &SerializableGraph,
        entry: &'a Observation,
        writing: bool,
    ) -> Self {
        Self::new(graph, Body::Predicate(entry, writing))
    }

    pub fn encoded_length(&self, control: &StorageReadControl) -> VersionResult<u64> {
        let mut bytes = CountBytes(0);
        self.write(&mut bytes, control)?;
        Ok(bytes.0)
    }

    pub fn write(&self, output: &mut dyn Write, control: &StorageReadControl) -> VersionResult<()> {
        let mut encoder = Encoder::new(output, control);
        encoder.bytes(MAGIC)?;
        encoder.bytes(&self.database.as_bytes())?;
        encoder.bytes(&self.coordinator)?;
        encoder.byte(match self.body {
            Body::Header(_) => 0,
            Body::Transaction(_) => 1,
            Body::Edge(_) => 2,
            Body::Predicate(_, false) => 3,
            Body::Predicate(_, true) => 4,
        })?;
        match self.body {
            Body::Header(graph) => {
                for value in [graph.last_allocation, graph.clock, graph.pending_finishes] {
                    encoder.number(value)?;
                }
                for count in [
                    graph.transactions.len(),
                    graph.outgoing.len(),
                    graph.predicates.reads.len(),
                    graph.predicates.writes.len(),
                ] {
                    encoder.count(count)?;
                }
            }
            Body::Transaction(entry) => nodes::write(&mut encoder, entry)?,
            Body::Edge(Edge(reader, writer)) => {
                encoder.number(reader)?;
                encoder.number(writer)?;
            }
            Body::Predicate(entry, _) => predicates::write(&mut encoder, entry)?,
        }
        encoder.finish()
    }

    pub(super) fn key(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<SerializableCheckpointKey> {
        Ok(match self.body {
            Body::Header(_) => SerializableCheckpointKey::HEADER,
            Body::Transaction(entry) => {
                SerializableCheckpointKey::transaction(entry.id, self.fingerprint(control)?)
            }
            Body::Edge(Edge(reader, writer)) => SerializableCheckpointKey::edge(reader, writer),
            Body::Predicate(entry, writing) => SerializableCheckpointKey::predicate(
                entry.predicate.object,
                entry.fingerprint,
                writing,
            ),
        })
    }

    pub(in crate::mvcc::serializable) fn fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<[u8; 32]> {
        let mut output = HashBytes(Sha256::new());
        self.write(&mut output, control)?;
        Ok(output.0.finalize().into())
    }
}

struct HashBytes(Sha256);

impl Write for HashBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
