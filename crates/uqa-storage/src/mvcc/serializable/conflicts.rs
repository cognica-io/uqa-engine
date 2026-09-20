//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dangerous-structure checks use prepare/commit order and read-only snapshot order.

use super::{edge_range, SerializableGraph, Transaction};
use crate::{mvcc::VersionResult, read_control::StorageReadControl};

impl SerializableGraph {
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
