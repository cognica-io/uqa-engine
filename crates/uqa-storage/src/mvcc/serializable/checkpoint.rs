//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned streaming checkpoints preserve the common SSI state across physical coordination owners.

mod io;
mod nodes;
mod predicates;
pub(super) mod records;
#[cfg(test)]
mod tests;

use std::io::{Read, Write};

use super::{DatabaseId, Edge, SerializableGraph, Transaction, VersionError, VersionResult};
use crate::read_control::StorageReadControl;
use io::{invalid, Decoder, Encoder};

const MAGIC: &[u8; 8] = b"UQASER03";

impl SerializableGraph {
    /// Whether retained state needs a durable checkpoint. A new coordinator does; a fully restored checkpoint does not until an operation changes its contents, including changes preceding an error. Encoding alone cannot acknowledge provider durability, so it never clears this flag. Check only while holding the same exclusive admission that loaded this graph.
    pub fn checkpoint_changed(&self) -> bool {
        self.checkpoint_changed
    }

    /// Stream one complete coordinator checkpoint, including predicates and prepared physical receipt bindings. The provider owns atomic replacement, encryption and shared admission. A checksum detects incomplete/corrupt state; it does not replace the provider's authentication or durability. No complete encoded-state buffer is allocated.
    pub fn write_checkpoint(
        &self,
        output: &mut dyn Write,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let mut encoder = Encoder::new(output, control);
        encoder.bytes(MAGIC)?;
        encoder.bytes(&self.database.as_bytes())?;
        encoder.bytes(&self.coordinator)?;
        for value in [self.last_allocation, self.clock, self.pending_finishes] {
            encoder.number(value)?;
        }
        for count in [
            self.transactions.len(),
            self.outgoing.len(),
            self.predicates.reads.len(),
            self.predicates.writes.len(),
        ] {
            encoder.count(count)?;
        }
        for &entry in &*self.transactions {
            nodes::write(&mut encoder, entry)?;
        }
        for &Edge(reader, writer) in &*self.outgoing {
            encoder.number(reader)?;
            encoder.number(writer)?;
        }
        for entries in [&self.predicates.reads, &self.predicates.writes] {
            for entry in &**entries {
                predicates::write(&mut encoder, entry)?;
            }
        }
        encoder.finish()
    }

    /// Compute the exact serialized length for bounded physical BLOB allocation without creating an encoded-state copy.
    pub fn checkpoint_length(&self, control: &StorageReadControl) -> VersionResult<u64> {
        let mut count = CountBytes(0);
        self.write_checkpoint(&mut count, control)?;
        Ok(count.0)
    }

    /// Restore a complete checkpoint for the expected database and coordinator incarnation. Decoded participants, both edge orientations and owned predicate keys share the supplied allowance. On any format, checksum, I/O, cancellation or resource failure, no partially restored graph is returned.
    pub fn read_checkpoint(
        database: DatabaseId,
        coordinator: [u8; 16],
        input: &mut dyn Read,
        control: &StorageReadControl,
    ) -> VersionResult<Self> {
        let mut decoder = Decoder::new(input, control);
        let magic = decoder.array::<8>()?;
        if magic != *MAGIC && magic != *b"UQASER02" && magic != *b"UQASER01" {
            return Err(invalid());
        }
        if decoder.array::<16>()? != database.as_bytes() {
            return Err(VersionError::WrongDatabase);
        }
        if decoder.array::<16>()? != coordinator {
            return Err(VersionError::WrongSerializableCoordinator);
        }
        let mut graph = Self::new(database, coordinator, control.memory())?;
        graph.last_allocation = decoder.number()?;
        graph.clock = decoder.number()?;
        graph.pending_finishes = decoder.number()?;
        if graph.clock.checked_add(graph.pending_finishes).is_none()
            || (graph.clock == 0 && graph.last_allocation != 0)
        {
            return Err(invalid());
        }
        let transaction_count = decoder.count()?;
        let edge_count = decoder.count()?;
        let read_count = decoder.count()?;
        let write_count = decoder.count()?;
        nodes::restore(
            &mut graph,
            transaction_count,
            magic != *b"UQASER01",
            magic == *MAGIC,
            &mut decoder,
        )?;
        graph.outgoing.reserve(edge_count)?;
        graph.incoming.reserve(edge_count)?;
        for _ in 0..edge_count {
            let edge = Edge(decoder.number()?, decoder.number()?);
            if edge.0 == edge.1
                || graph
                    .outgoing
                    .last()
                    .is_some_and(|previous| *previous >= edge)
            {
                return Err(invalid());
            }
            lookup(&graph, edge.0)?;
            if lookup(&graph, edge.1)?.read_only {
                return Err(invalid());
            }
            graph.outgoing.push(edge)?;
            graph.incoming.push(Edge(edge.1, edge.0))?;
        }
        graph.incoming.sort_unstable();
        for (writing, count) in [(false, read_count), (true, write_count)] {
            let entries = if writing {
                &mut graph.predicates.writes
            } else {
                &mut graph.predicates.reads
            };
            entries.reserve(count)?;
            for _ in 0..count {
                let mut observed = predicates::read(&mut decoder, writing)?;
                observed.fingerprint =
                    records::SerializableCheckpointRecord::predicate(&graph, &observed, writing)
                        .fingerprint(control)?;
                let owner = lookup(&graph, observed.owner)?;
                if owner.aborted
                    || (writing
                        && (owner.read_only
                            || observed.write == 0
                            || observed.write > owner.writes))
                    || (!writing && observed.write != 0)
                {
                    return Err(invalid());
                }
                let entries = if writing {
                    &mut graph.predicates.writes
                } else {
                    &mut graph.predicates.reads
                };
                if entries
                    .last()
                    .is_some_and(|previous| previous.predicate.object > observed.predicate.object)
                {
                    return Err(invalid());
                }
                entries.push(observed)?;
            }
        }
        decoder.finish()?;
        for entries in [&mut graph.predicates.reads, &mut graph.predicates.writes] {
            entries.sort_unstable_by_key(|entry| (entry.predicate.object, entry.fingerprint));
        }
        control.check()?;
        graph.checkpoint_changed = false;
        Ok(graph)
    }
}

fn lookup(graph: &SerializableGraph, allocation: u64) -> VersionResult<&Transaction> {
    let position = graph
        .transactions
        .binary_search_by_key(&allocation, |entry| entry.id)
        .map_err(|_| invalid())?;
    Ok(&graph.transactions[position])
}

struct CountBytes(u64);

impl Write for CountBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(
                u64::try_from(bytes.len())
                    .map_err(|_| std::io::Error::other("checkpoint length overflow"))?,
            )
            .ok_or_else(|| std::io::Error::other("checkpoint length overflow"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
