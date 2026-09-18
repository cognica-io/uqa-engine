//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Three retained cluster views yield per-document changes, never a union of stale whole clusters.

use super::{
    resolve::{revision, Resolver},
    OccurrenceRecordValue as Value,
};
use crate::clustered_postings::{
    decode_occurrence_cluster_budgeted, encode_occurrence_cluster_controlled,
    validate_occurrence_cluster, OccurrencePosting,
};
use crate::mvcc::{
    CommittedRecordSnapshot, PreparedRecordWrite, SharedRecordValue, VersionError, VersionResult,
};
use uqa_core::memory::{Budgeted, BudgetedVec};

fn borrowed(value: Option<&SharedRecordValue>) -> Option<&[u8]> {
    value.map(|value| &***value)
}

impl Resolver<'_> {
    fn cluster_view(
        &self,
        snapshot: &dyn CommittedRecordSnapshot,
        write: &PreparedRecordWrite,
        peer: Option<&PreparedRecordWrite>,
    ) -> VersionResult<(Option<SharedRecordValue>, Option<SharedRecordValue>)> {
        let value = snapshot
            .get(write.key(), self.control)?
            .and_then(|row| row.into_parts().1);
        let peer = peer
            .map(|peer| snapshot.get(peer.key(), self.control))
            .transpose()?
            .flatten()
            .and_then(|row| row.into_parts().1);
        Ok((value, peer))
    }

    fn encoded_cluster<'a>(
        &self,
        key: &[u8],
        value: Option<&'a [u8]>,
        peer: Option<(&[u8], Option<&'a [u8]>)>,
    ) -> VersionResult<Option<(&'a [u8], &'a [u8])>> {
        let value = value
            .map(|value| self.layout.decode(key, value, self.control))
            .transpose()?;
        let peer = peer
            .map(|(key, value)| {
                value
                    .map(|value| self.layout.decode(key, value, self.control))
                    .transpose()
            })
            .transpose()?
            .flatten();
        let (score, positions) = match (value, peer) {
            (None, None) => return Ok(None),
            (Some(Value::Cluster { score, positions }), None)
            | (Some(Value::Score(score)), Some(Value::Positions(positions))) => (score, positions),
            _ => {
                return Err(VersionError::InvalidEncoding(
                    "incomplete occurrence cluster pair",
                ))
            }
        };
        Ok(Some((score, positions)))
    }

    fn decode_cluster(
        &self,
        cluster: u64,
        key: &[u8],
        value: Option<&[u8]>,
        peer: Option<(&[u8], Option<&[u8]>)>,
    ) -> VersionResult<Budgeted<Vec<OccurrencePosting>>> {
        let Some((score, positions)) = self.encoded_cluster(key, value, peer)? else {
            return Ok(Budgeted::new(
                Vec::new(),
                self.control.memory().empty_reservation(),
            ));
        };
        Ok(decode_occurrence_cluster_budgeted(
            cluster,
            score,
            positions,
            self.control.memory(),
            || {
                self.control.cancellation().check()?;
                Ok(())
            },
        )?)
    }

    fn select_postings<'a>(
        &self,
        base: &'a [OccurrencePosting],
        evaluated: &'a [OccurrencePosting],
        current: &'a [OccurrencePosting],
        mutation: usize,
        write: &PreparedRecordWrite,
    ) -> VersionResult<BudgetedVec<&'a OccurrencePosting>> {
        let mut merged = BudgetedVec::new(self.control.memory());
        merged.reserve(
            current
                .len()
                .checked_add(evaluated.len())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        let mut base = base.iter().peekable();
        let mut evaluated = evaluated.iter().peekable();
        let mut current = current.iter().peekable();
        loop {
            self.control.cancellation().check()?;
            let Some(id) = [base.peek(), evaluated.peek(), current.peek()]
                .into_iter()
                .flatten()
                .map(|entry| entry.doc_id)
                .min()
            else {
                break;
            };
            let before = base.next_if(|entry| entry.doc_id == id);
            let after = evaluated.next_if(|entry| entry.doc_id == id);
            let latest = current.next_if(|entry| entry.doc_id == id);
            let chosen = if before == after {
                latest
            } else {
                if before != latest {
                    return Err(VersionError::WriteConflict {
                        mutation,
                        expected: write.expected(),
                        actual: revision(self.current, write.key(), self.control)?,
                    });
                }
                after
            };
            if let Some(entry) = chosen {
                merged.push(entry)?;
            }
        }
        Ok(merged)
    }

    pub(super) fn merge_cluster(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
        peer: Option<&PreparedRecordWrite>,
        cluster: u64,
    ) -> VersionResult<()> {
        let (base_value, base_peer) = self.cluster_view(self.base, write, peer)?;
        if self.base.sequence() == self.current.sequence() {
            // The publication boundary has not advanced since evaluation. Validate both complete graphs, then share the evaluated bytes instead of decoding, merging and encoding them again.
            for (value, peer_value) in [
                (borrowed(base_value.as_ref()), borrowed(base_peer.as_ref())),
                (write.value(), peer.and_then(PreparedRecordWrite::value)),
            ] {
                if let Some((score, positions)) = self.encoded_cluster(
                    write.key(),
                    value,
                    peer.map(|peer| (peer.key(), peer_value)),
                )? {
                    validate_occurrence_cluster(cluster, score, positions, || {
                        self.control.cancellation().check()?;
                        Ok(())
                    })?;
                }
            }
            self.preserve(write)?;
            if let Some(peer) = peer {
                self.preserve(peer)?;
            }
            return Ok(());
        }
        let (current_value, current_peer) = self.cluster_view(self.current, write, peer)?;
        let base = self.decode_cluster(
            cluster,
            write.key(),
            borrowed(base_value.as_ref()),
            peer.map(|peer| (peer.key(), borrowed(base_peer.as_ref()))),
        )?;
        let evaluated = self.decode_cluster(
            cluster,
            write.key(),
            write.value(),
            peer.map(|peer| (peer.key(), peer.value())),
        )?;
        let current = self.decode_cluster(
            cluster,
            write.key(),
            borrowed(current_value.as_ref()),
            peer.map(|peer| (peer.key(), borrowed(current_peer.as_ref()))),
        )?;
        let merged = self.select_postings(&base, &evaluated, &current, mutation, write)?;
        if merged.is_empty() {
            self.replace(write.key(), None)?;
            if let Some(peer) = peer {
                self.replace(peer.key(), None)?;
            }
        } else {
            let template = write
                .value()
                .or_else(|| borrowed(current_value.as_ref()))
                .or_else(|| borrowed(base_value.as_ref()))
                .expect("nonempty merged cluster");
            let (score, positions) =
                encode_occurrence_cluster_controlled(merged.iter().copied(), self.control)?;
            if let Some(peer) = peer {
                self.encode_replace(write.key(), template, Value::Score(&score))?;
                self.encode_replace(
                    peer.key(),
                    peer.value().unwrap_or_default(),
                    Value::Positions(&positions),
                )?;
            } else {
                self.encode_replace(
                    write.key(),
                    template,
                    Value::Cluster {
                        score: &score,
                        positions: &positions,
                    },
                )?;
            }
        }
        Ok(())
    }
}
