//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical conditions and unchanged combined clusters share their read windows; other derived merges close them before provider reads.

use super::{
    Kind, PreparedLookup, PreparedRecordCommit, PreparedRecordWrite, RecordWriteKind,
    ResolutionMode, Resolver, Source, VersionError, VersionResult,
};
use crate::mvcc::{PreparedWriteCursor, RecordMetadata, RecordMetadataRequests};
use std::cell::RefCell;
use uqa_core::memory::BudgetedVec;

enum Pending {
    Validation(PreparedRecordWrite),
    Merge(PreparedRecordWrite, Source),
}

pub(super) fn apply<'p>(
    original: &'p PreparedRecordCommit,
    lookup: &PreparedLookup<'p>,
    resolver: &Resolver<'_>,
) -> VersionResult<()> {
    let mut stream = Stream {
        resolver,
        lookup,
        input: original.writes(),
        pending: None,
        mutation: 0,
    };
    loop {
        resolver
            .current
            .visit_metadata(&mut stream, resolver.control)?;
        let Some(Pending::Merge(write, source)) = stream.pending.take() else {
            return Ok(());
        };
        if resolver.base.sequence() == resolver.current.sequence()
            && write.kind() == RecordWriteKind::Occurrence
            && matches!(source.kind, Kind::Cluster(_))
        {
            stream.pending = Some(Pending::Merge(write, source));
            preserve_clusters(&mut stream)?;
        } else {
            resolver.merge_write(stream.mutation, &write, lookup, source)?;
            stream.mutation += 1;
        }
    }
}

struct Stream<'a, 'p> {
    resolver: &'a Resolver<'a>,
    lookup: &'a PreparedLookup<'p>,
    input: PreparedWriteCursor<'p>,
    pending: Option<Pending>,
    mutation: usize,
}

impl RecordMetadataRequests for Stream<'_, '_> {
    fn advance(&mut self) -> VersionResult<bool> {
        // A combined-cluster visitor can stop before a different derived effect
        // or a command's canonical validation. Resume that exact pending record.
        if let Some(pending) = &self.pending {
            return Ok(matches!(pending, Pending::Validation(_)));
        }
        while let Some(mut write) = self.input.next(self.resolver.control)? {
            let validate = match write.kind() {
                RecordWriteKind::Canonical => true,
                RecordWriteKind::GraphCache
                | RecordWriteKind::GraphPreview
                | RecordWriteKind::IVFPreview
                | RecordWriteKind::HNSWPreview
                | RecordWriteKind::Marker
                | RecordWriteKind::DiskANNOrigin
                | RecordWriteKind::DiskANNPopulationPreview
                | RecordWriteKind::IdempotentDelete
                | RecordWriteKind::StatisticsMaintenance => false,
                RecordWriteKind::Occurrence | RecordWriteKind::OccurrenceCache => {
                    let source = self.resolver.source(&write, self.lookup)?;
                    if !source.structural {
                        self.pending = Some(Pending::Merge(write, source));
                        return Ok(false);
                    }
                    write = write.with_kind(RecordWriteKind::Canonical);
                    true
                }
            };
            if validate && self.resolver.mode == ResolutionMode::Command {
                self.pending = Some(Pending::Validation(write));
                return Ok(true);
            }
            self.resolver
                .changes
                .preserve(write, self.resolver.control)?;
            self.mutation += 1;
        }
        Ok(false)
    }

    fn key(&self) -> &[u8] {
        let Some(Pending::Validation(write)) = &self.pending else {
            unreachable!("only canonical conditions request metadata")
        };
        write.key()
    }

    fn accept(&mut self, metadata: Option<RecordMetadata>) -> VersionResult<()> {
        let Some(Pending::Validation(write)) = self.pending.take() else {
            unreachable!("only canonical conditions receive metadata")
        };
        let actual = metadata.and_then(|record| record.revision);
        if actual != write.expected() {
            return Err(VersionError::WriteConflict {
                mutation: self.mutation,
                expected: write.expected(),
                actual,
            });
        }
        self.resolver
            .changes
            .preserve(write, self.resolver.control)?;
        self.mutation += 1;
        Ok(())
    }
}

/// At one committed boundary, combined clusters need only validation of their
/// complete base/evaluated graphs. Share physical admission and retain their
/// input order; no related key is written and no committed provider is reentered.
fn preserve_clusters(stream: &mut Stream<'_, '_>) -> VersionResult<()> {
    let resolver = stream.resolver;
    let stream = RefCell::new(stream);
    let selected = RefCell::new(None);
    let mut keys = std::iter::from_fn(|| {
        let next = || -> VersionResult<Option<BudgetedVec<u8>>> {
            resolver.control.check()?;
            let mut stream = stream.borrow_mut();
            stream.advance()?;
            let (write, cluster, marker, marker_kind, fence) = match stream.pending.take() {
                Some(Pending::Merge(
                    write,
                    Source {
                        kind: Kind::Cluster(cluster),
                        marker,
                        marker_kind,
                        fence,
                        ..
                    },
                )) if write.kind() == RecordWriteKind::Occurrence => {
                    (write, cluster, marker, marker_kind, fence)
                }
                pending => {
                    stream.pending = pending;
                    return Ok(None);
                }
            };
            resolver.validate_marker(&marker, marker_kind, stream.lookup)?;
            resolver.validate_fence(stream.mutation, fence)?;
            let mut key = BudgetedVec::new(resolver.control.memory());
            key.extend_from_slice(write.key())?;
            *selected.borrow_mut() = Some((write, cluster));
            Ok(Some(key))
        };
        next().transpose()
    });
    resolver
        .base
        .visit_values(&mut keys, resolver.control, &mut |_, base| {
            let (write, cluster) = selected.borrow_mut().take().expect("requested cluster");
            resolver.validate_cluster(
                cluster,
                write.key(),
                base.and_then(|row| row.value),
                None,
            )?;
            resolver.validate_cluster(cluster, write.key(), write.value(), None)?;
            let kind = resolver.mode.kind(write.kind());
            resolver
                .changes
                .preserve(write.with_kind(kind), resolver.control)?;
            stream.borrow_mut().mutation += 1;
            Ok(true)
        })
}
