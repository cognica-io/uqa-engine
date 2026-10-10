//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consecutive canonical conditions share admission; derived merges close it before provider reads.

use super::{
    PreparedLookup, PreparedRecordCommit, PreparedRecordWrite, RecordWriteKind, ResolutionMode,
    Resolver, Source, VersionError, VersionResult,
};
use crate::mvcc::{PreparedWriteCursor, RecordMetadata, RecordMetadataRequests};

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
        resolver.merge_write(stream.mutation, &write, lookup, source)?;
        stream.mutation += 1;
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
