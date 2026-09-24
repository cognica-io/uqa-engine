//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Checked field-count deltas retain the analyzer revision selected during evaluation.

use super::{
    resolve::{bytes, revision, Resolver},
    OccurrenceRecordValue as Value,
};
use crate::inverted_index::IndexedFieldRevision;
use crate::mvcc::{PreparedRecordWrite, VersionError, VersionResult};

#[derive(Clone, Copy)]
struct Statistics {
    revision: IndexedFieldRevision,
    doc_count: u64,
    total_length: u64,
}

impl Resolver<'_> {
    fn statistics(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<Option<Statistics>> {
        value
            .map(
                |value| match self.layout.decode(key, value, self.control)? {
                    Value::Statistics {
                        revision,
                        doc_count,
                        total_length,
                    } => Ok(Statistics {
                        revision,
                        doc_count,
                        total_length,
                    }),
                    _ => Err(VersionError::InvalidEncoding(
                        "invalid occurrence statistics projection",
                    )),
                },
            )
            .transpose()
    }

    pub(super) fn merge_statistics(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
    ) -> VersionResult<()> {
        let base = self.base.get(write.key(), self.control)?;
        let current = self.current.get(write.key(), self.control)?;
        let before = self.statistics(write.key(), bytes(base.as_ref()))?;
        let after = self.statistics(write.key(), write.value())?;
        let latest = self.statistics(write.key(), bytes(current.as_ref()))?;
        let mut revisions = [before, after, latest]
            .into_iter()
            .flatten()
            .map(|stats| stats.revision);
        let Some(revision_value) = revisions.next() else {
            return self.replace(write.key(), None);
        };
        if revisions.any(|candidate| candidate != revision_value) {
            return Err(VersionError::WriteConflict {
                mutation,
                expected: write.expected(),
                actual: revision(self.current, write.key(), self.control)?,
            });
        }
        let delta = |select: fn(Statistics) -> u64| -> VersionResult<u64> {
            let total = i128::from(latest.map_or(0, select)) + i128::from(after.map_or(0, select))
                - i128::from(before.map_or(0, select));
            u64::try_from(total).map_err(|_| {
                VersionError::InvalidEncoding("occurrence field total overflow or underflow")
            })
        };
        let doc_count = delta(|stats| stats.doc_count)?;
        let total_length = delta(|stats| stats.total_length)?;
        if doc_count == 0 {
            if total_length != 0 {
                return Err(VersionError::InvalidEncoding(
                    "empty occurrence field retains length",
                ));
            }
            self.replace(write.key(), None)
        } else {
            let template = write
                .value()
                .or_else(|| bytes(current.as_ref()))
                .or_else(|| bytes(base.as_ref()))
                .expect("nonempty field statistics");
            self.encode_replace(
                write.key(),
                template,
                Value::Statistics {
                    revision: revision_value,
                    doc_count,
                    total_length,
                },
            )
        }
    }
}
