//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate unchanged combined or split clusters in one bounded read window.

use std::cell::RefCell;
use uqa_core::memory::BudgetedVec;

use super::{
    Kind, Pending, PreparedRecordWrite, RecordWriteKind, Stream, VersionError, VersionResult,
};
use crate::mvcc::occurrence::OccurrenceRelatedKey as Related;
use crate::mvcc::RecordMetadataRequests;

struct Pair {
    write: PreparedRecordWrite,
    peer: PreparedRecordWrite,
    cluster: u64,
}

enum Selected {
    Cluster(PreparedRecordWrite, u64),
    Score(Pair),
    Positions(Pair, Option<BudgetedVec<u8>>),
}

impl Selected {
    fn key(&self) -> &[u8] {
        match self {
            Self::Cluster(write, _) => write.key(),
            Self::Score(pair) => pair.write.key(),
            Self::Positions(pair, _) => pair.peer.key(),
        }
    }
}

pub(super) fn apply(stream: &mut Stream<'_, '_>) -> VersionResult<()> {
    let resolver = stream.resolver;
    let stream = RefCell::new(stream);
    let selected = RefCell::new(None);
    let mut keys = std::iter::from_fn(|| {
        let next = || -> VersionResult<Option<BudgetedVec<u8>>> {
            resolver.control.check()?;
            let mut selection = selected.borrow_mut();
            if selection.is_none() {
                *selection = select(&mut stream.borrow_mut())?;
            }
            let Some(selection) = selection.as_ref() else {
                return Ok(None);
            };
            let mut key = BudgetedVec::new(resolver.control.memory());
            key.extend_from_slice(selection.key())?;
            Ok(Some(key))
        };
        next().transpose()
    });
    resolver
        .base
        .visit_values(&mut keys, resolver.control, &mut |_, base| {
            let selection = selected
                .borrow_mut()
                .take()
                .expect("requested occurrence record");
            let base = base.and_then(|row| row.value);
            match selection {
                Selected::Score(pair) => {
                    let score = base
                        .map(|value| {
                            let mut bytes = BudgetedVec::new(resolver.control.memory());
                            bytes.extend_from_slice(value)?;
                            Ok::<_, VersionError>(bytes)
                        })
                        .transpose()?;
                    *selected.borrow_mut() = Some(Selected::Positions(pair, score));
                    return Ok(true);
                }
                Selected::Cluster(write, cluster) => {
                    resolver.validate_cluster(cluster, write.key(), base, None)?;
                    resolver.validate_cluster(cluster, write.key(), write.value(), None)?;
                    resolver.preserve(&write)?;
                }
                Selected::Positions(pair, score) => {
                    resolver.validate_cluster(
                        pair.cluster,
                        pair.write.key(),
                        score.as_deref(),
                        Some((pair.peer.key(), base)),
                    )?;
                    resolver.validate_cluster(
                        pair.cluster,
                        pair.write.key(),
                        pair.write.value(),
                        Some((pair.peer.key(), pair.peer.value())),
                    )?;
                    resolver.preserve(&pair.write)?;
                }
            }
            stream.borrow_mut().mutation += 1;
            Ok(true)
        })
}

fn select(stream: &mut Stream<'_, '_>) -> VersionResult<Option<Selected>> {
    let resolver = stream.resolver;
    loop {
        stream.advance()?;
        let (write, source) = match stream.pending.take() {
            Some(Pending::Merge(write, source))
                if write.kind() == RecordWriteKind::Occurrence
                    && matches!(
                        source.kind,
                        Kind::Cluster(_) | Kind::Score(_) | Kind::Positions
                    ) =>
            {
                (write, source)
            }
            pending => {
                stream.pending = pending;
                return Ok(None);
            }
        };
        resolver.validate_marker(&source.marker, source.marker_kind, stream.lookup)?;
        let peer = if matches!(source.kind, Kind::Score(_) | Kind::Positions) {
            let peer_key = resolver.layout.related_key(
                write.key(),
                if source.kind == Kind::Positions {
                    Related::Score
                } else {
                    Related::Positions
                },
                resolver.control,
            )?;
            let peer = stream.lookup.get(&peer_key, resolver.control)?.ok_or(
                VersionError::InvalidEncoding("unpaired occurrence cluster replacement"),
            )?;
            if peer.kind() != RecordWriteKind::Occurrence {
                // Canonical peer conditions need the ordinary current-view validation outside this window.
                stream.pending = Some(Pending::Merge(write, source));
                return Ok(None);
            }
            Some(peer)
        } else {
            None
        };
        resolver.validate_fence(stream.mutation, source.fence)?;
        match source.kind {
            Kind::Cluster(cluster) => return Ok(Some(Selected::Cluster(write, cluster))),
            Kind::Score(cluster) => {
                return Ok(Some(Selected::Score(Pair {
                    write,
                    peer: peer.expect("paired score"),
                    cluster,
                })))
            }
            Kind::Positions => {
                // The score entry validates the complete pair. Emit this original input only at its own key, regardless of the relative key order.
                resolver.preserve(&write)?;
                stream.mutation += 1;
            }
            _ => unreachable!("selected cluster kind"),
        }
    }
}
