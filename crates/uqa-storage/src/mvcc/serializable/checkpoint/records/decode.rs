//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore and validate an ordered record set before exposing its graph or comparison baseline.

use std::io::Read;

use sha2::{Digest, Sha256};

use super::super::{io::Decoder, lookup, nodes, predicates};
use super::{invalid, SerializableCheckpointKey, SerializableGraph, VersionResult, MAGIC};
use crate::{
    mvcc::{
        serializable::{observations::Observation, Edge, Transaction},
        DatabaseId, VersionError,
    },
    read_control::StorageReadControl,
};

impl SerializableGraph {
    /// Restore a complete ordered checkpoint record set for this database incarnation. The scanner supplies each key and one bounded value reader exactly once in byte-key order, stopping on a callback error. Admission retains the first decode error even if the scanner suppresses or replaces that stop signal; subsequent callbacks read nothing. Checksums, identities, keys, counts and graph references must all agree. The graph, owned predicates and retained comparison keys share the caller's allowance; no partial restore escapes on error.
    pub fn read_checkpoint_records(
        database: DatabaseId,
        coordinator: [u8; 16],
        control: &StorageReadControl,
        mut scan: impl FnMut(
            &mut dyn FnMut(&[u8], &mut dyn Read) -> VersionResult<()>,
        ) -> VersionResult<()>,
    ) -> VersionResult<Self> {
        control.check()?;
        let mut graph = Self::new(database, coordinator, control.memory())?;
        let mut expected = [0; 4];
        let mut failure = None;
        let scanned = scan(&mut |key, input| {
            if failure.is_some() {
                return Err(invalid());
            }
            let result = (|| {
                control.check()?;
                let key = SerializableCheckpointKey::from_bytes(key)?;
                if graph
                    .checkpoint_records
                    .last()
                    .is_some_and(|previous| *previous >= key)
                    || (graph.checkpoint_records.is_empty()
                        && key != SerializableCheckpointKey::HEADER)
                {
                    return Err(invalid());
                }
                restore_record(&mut graph, key, input, &mut expected, control)?;
                graph.checkpoint_records.push(key)?;
                Ok(())
            })();
            result.map_err(|error| {
                failure = Some(error);
                invalid()
            })
        });
        if let Some(error) = failure {
            return Err(error);
        }
        scanned?;
        control.check()?;
        if graph.checkpoint_records.is_empty()
            || expected
                != [
                    graph.transactions.len(),
                    graph.outgoing.len(),
                    graph.predicates.reads.len(),
                    graph.predicates.writes.len(),
                ]
        {
            return Err(invalid());
        }
        let pending = graph
            .transactions
            .iter()
            .try_fold(0_u64, |pending, entry| {
                pending
                    .checked_add(u64::from(entry.prepared.is_some() && entry.live()))
                    .ok_or_else(invalid)
            })?;
        if pending != graph.pending_finishes {
            return Err(invalid());
        }
        graph.incoming.sort_unstable();
        control.check()?;
        graph.checkpoint_changed = false;
        Ok(graph)
    }
}

enum DecodedRecord {
    Header,
    Transaction(Transaction),
    Edge(Edge),
    Predicate(Observation, bool),
}

fn restore_record(
    graph: &mut SerializableGraph,
    key: SerializableCheckpointKey,
    input: &mut dyn Read,
    expected: &mut [usize; 4],
    control: &StorageReadControl,
) -> VersionResult<()> {
    let mut checked = HashRead {
        input,
        digest: Sha256::new(),
    };
    let mut decoder = Decoder::new(&mut checked, control);
    if decoder.array::<8>()? != *MAGIC {
        return Err(invalid());
    }
    if decoder.array::<16>()? != graph.database.as_bytes() {
        return Err(VersionError::WrongDatabase);
    }
    if decoder.array::<16>()? != graph.coordinator {
        return Err(VersionError::WrongSerializableCoordinator);
    }
    if decoder.byte()? != key.kind() {
        return Err(invalid());
    }
    let record = match key.kind() {
        0 => {
            read_header(graph, expected, &mut decoder)?;
            DecodedRecord::Header
        }
        1 => DecodedRecord::Transaction(read_transaction(graph, expected[0], &mut decoder)?),
        2 => DecodedRecord::Edge(read_edge(graph, expected[1], &mut decoder)?),
        3 | 4 => {
            let writing = key.kind() == 4;
            DecodedRecord::Predicate(
                read_predicate(
                    graph,
                    expected[if writing { 3 } else { 2 }],
                    writing,
                    &mut decoder,
                )?,
                writing,
            )
        }
        _ => return Err(invalid()),
    };
    decoder.finish()?;
    let fingerprint = checked.digest.finalize().into();
    let canonical = match &record {
        DecodedRecord::Header => SerializableCheckpointKey::HEADER,
        DecodedRecord::Transaction(entry) => {
            SerializableCheckpointKey::transaction(entry.id, fingerprint)
        }
        DecodedRecord::Edge(Edge(reader, writer)) => {
            SerializableCheckpointKey::edge(*reader, *writer)
        }
        DecodedRecord::Predicate(entry, writing) => {
            SerializableCheckpointKey::predicate(entry.predicate.object, fingerprint, *writing)
        }
    };
    if key != canonical {
        return Err(invalid());
    }
    match record {
        DecodedRecord::Header => {}
        DecodedRecord::Transaction(entry) => graph.transactions.push(entry)?,
        DecodedRecord::Edge(Edge(reader, writer)) => {
            graph.outgoing.push(Edge(reader, writer))?;
            graph.incoming.push(Edge(writer, reader))?;
        }
        DecodedRecord::Predicate(mut entry, writing) => {
            entry.fingerprint = fingerprint;
            let entries = if writing {
                &mut graph.predicates.writes
            } else {
                &mut graph.predicates.reads
            };
            entries.push(entry)?;
        }
    }
    Ok(())
}

fn read_header(
    graph: &mut SerializableGraph,
    expected: &mut [usize; 4],
    decoder: &mut Decoder<'_>,
) -> VersionResult<()> {
    graph.last_allocation = decoder.number()?;
    graph.clock = decoder.number()?;
    graph.pending_finishes = decoder.number()?;
    if graph.clock.checked_add(graph.pending_finishes).is_none()
        || (graph.clock == 0 && graph.last_allocation != 0)
    {
        return Err(invalid());
    }
    for count in expected.iter_mut() {
        *count = decoder.count()?;
    }
    let total = expected.iter().try_fold(1_usize, |total, count| {
        total.checked_add(*count).ok_or_else(invalid)
    })?;
    graph.checkpoint_records.reserve(total)?;
    graph.transactions.reserve(expected[0])?;
    graph.outgoing.reserve(expected[1])?;
    graph.incoming.reserve(expected[1])?;
    graph.predicates.reads.reserve(expected[2])?;
    graph.predicates.writes.reserve(expected[3])?;
    Ok(())
}

fn read_transaction(
    graph: &SerializableGraph,
    expected: usize,
    decoder: &mut Decoder<'_>,
) -> VersionResult<Transaction> {
    if graph.transactions.len() >= expected {
        return Err(invalid());
    }
    let entry = nodes::read(decoder, graph.clock, graph.last_allocation, true, true)?;
    if graph
        .transactions
        .last()
        .is_some_and(|previous| previous.id >= entry.id)
    {
        return Err(invalid());
    }
    Ok(entry)
}

fn read_edge(
    graph: &SerializableGraph,
    expected: usize,
    decoder: &mut Decoder<'_>,
) -> VersionResult<Edge> {
    if graph.outgoing.len() >= expected {
        return Err(invalid());
    }
    let Edge(reader, writer) = Edge(decoder.number()?, decoder.number()?);
    if reader == writer {
        return Err(invalid());
    }
    lookup(graph, reader)?;
    if lookup(graph, writer)?.read_only {
        return Err(invalid());
    }
    Ok(Edge(reader, writer))
}

fn read_predicate(
    graph: &SerializableGraph,
    expected: usize,
    writing: bool,
    decoder: &mut Decoder<'_>,
) -> VersionResult<Observation> {
    let entries = if writing {
        &graph.predicates.writes
    } else {
        &graph.predicates.reads
    };
    if entries.len() >= expected {
        return Err(invalid());
    }
    let observed = predicates::read(decoder, writing)?;
    let owner = lookup(graph, observed.owner)?;
    if owner.aborted
        || (writing && (owner.read_only || observed.write == 0 || observed.write > owner.writes))
        || (!writing && observed.write != 0)
    {
        return Err(invalid());
    }
    Ok(observed)
}

struct HashRead<'a> {
    input: &'a mut dyn Read,
    digest: Sha256,
}

impl Read for HashRead<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.digest.update(&bytes[..count]);
        Ok(count)
    }
}
