//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dangerous-structure checks use prepare/commit order and read-only snapshot order.

use super::{edge_range, insert_edge, Edge, SerializableGraph, Transaction};
use crate::{
    mvcc::{SerializableTransactionId, VersionError, VersionResult},
    read_control::StorageReadControl,
};

#[derive(Clone, Copy)]
pub(super) struct DependencyAction {
    pub(super) edge: Edge,
    pub(super) victim: Option<usize>,
}

impl SerializableGraph {
    pub(super) fn plan_dependency(
        &self,
        observer: SerializableTransactionId,
        reader: SerializableTransactionId,
        writer: SerializableTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<Option<DependencyAction>> {
        control.check()?;
        self.check_active(observer)?;
        if observer != reader && observer != writer {
            return Err(VersionError::InvalidEncoding(
                "serializable observer is not a dependency endpoint",
            ));
        }
        let reader_position = self.position(reader)?;
        let writer_position = self.position(writer)?;
        let read = self.transactions[reader_position];
        let write = self.transactions[writer_position];
        if reader == writer || !read.relevant() || !write.relevant() {
            return Ok(None);
        }
        if write.read_only {
            return Err(VersionError::InvalidEncoding(
                "serializable dependency targets a read-only writer",
            ));
        }
        if write.committed.is_some_and(|end| end <= read.snapshot)
            || read.committed.is_some_and(|end| end <= write.snapshot)
        {
            return Ok(None);
        }
        let edge = Edge(read.id, write.id);
        if self.outgoing.binary_search(&edge).is_ok() {
            return Ok(None);
        }
        let victim = if self.dangerous_edge(read, write, control)? {
            let victim = if write.prepared.is_some() {
                reader_position
            } else {
                writer_position
            };
            if self.transactions[victim].id == observer.allocation() {
                return Err(VersionError::SerializationConflict {
                    transaction: observer,
                });
            }
            Some(victim)
        } else {
            None
        };
        Ok(Some(DependencyAction { edge, victim }))
    }

    pub(super) fn reserve_dependencies(&mut self, count: usize) -> VersionResult<()> {
        self.outgoing.reserve(count)?;
        self.incoming.reserve(count)?;
        Ok(())
    }

    pub(super) fn publish_dependency(&mut self, action: DependencyAction) -> VersionResult<()> {
        if let Some(victim) = action.victim {
            self.transactions[victim].doomed = true;
        } else {
            insert_edge(&mut self.outgoing, action.edge)?;
            insert_edge(&mut self.incoming, Edge(action.edge.1, action.edge.0))?;
        }
        Ok(())
    }

    pub(super) fn dangerous_edge(
        &self,
        reader: Transaction,
        writer: Transaction,
        control: &StorageReadControl,
    ) -> VersionResult<bool> {
        // reader -> writer -> outgoing: only an outgoing participant that prepared first can close the structure.
        if let Some(order) = self.earliest_out(writer, control)? {
            if reader.committed.is_none_or(|end| order <= end)
                && writer.committed.is_none_or(|end| order <= end)
                && (!reader.read_only || order <= reader.snapshot)
            {
                return Ok(true);
            }
        }
        // incoming -> reader -> writer: a prepared writer is irrevocable, so examine the reader's incoming dependencies too.
        if let Some(order) = writer.prepared {
            for edge in &self.incoming[edge_range(&self.incoming, reader.id)] {
                control.check()?;
                let incoming = self.node(edge.1);
                if incoming.relevant()
                    && incoming.committed.is_none_or(|end| order <= end)
                    && (!incoming.read_only || order <= incoming.snapshot)
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn dangerous_pivot(
        &self,
        committing: u64,
        pivot: Transaction,
        control: Option<&StorageReadControl>,
    ) -> VersionResult<bool> {
        if !pivot.relevant() || !pivot.live() {
            return Ok(false);
        }
        for edge in &self.incoming[edge_range(&self.incoming, pivot.id)] {
            if let Some(control) = control {
                control.check()?;
            }
            let incoming = self.node(edge.1);
            if incoming.relevant()
                && (incoming.id == committing || (incoming.live() && !incoming.read_only))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn earliest_out(
        &self,
        transaction: Transaction,
        control: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        let mut earliest = transaction.summarized_out;
        for edge in &self.outgoing[edge_range(&self.outgoing, transaction.id)] {
            control.check()?;
            let target = self.node(edge.1);
            if target.relevant() {
                if let Some(order) = target.prepared {
                    earliest = Some(earliest.map_or(order, |old| old.min(order)));
                }
            }
        }
        Ok(earliest)
    }
}
