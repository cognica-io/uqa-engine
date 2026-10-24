//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final named results borrow stable decoded scan fields until their selected values are owned.

use super::{ScoredDocumentSource, ScoredInputCursor};
use crate::{query::output::MaterializedResultSink, ExecResult};

impl ScoredDocumentSource {
    pub(super) fn collect_shared_public_rows(
        &mut self,
        sink: &mut MaterializedResultSink<'_>,
    ) -> ExecResult<bool> {
        if self.lock_origin.is_some()
            || self.recheck_pinned
            || self.projection_properties.requires_materialization
            || !self.projection_properties.identity
            || self.appended_doc_id_attribute.is_some()
            || self.appended_score_attribute.is_some()
            || self.schema.physical_width() != sink.physical_width()
            || self.projected_fields.len() != self.schema.physical_width()
        {
            return Ok(false);
        }
        let ScoredInputCursor::All { mut after } = self.input else {
            return Ok(false);
        };
        if self
            .table
            .read_documents()
            .for_each_next_shared_fields_borrowed(after, 0, &[], &mut |_, _| {
                unreachable!("zero-limit projection invoked a visitor")
            })
            .is_none()
        {
            return Ok(false);
        }
        let fields = self
            .projected_fields
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        loop {
            sink.check_cancelled()?;
            self.serializable.observe_relation()?;
            let mut size = None;
            let mut last = None;
            let mut failure = None;
            let mut largest_row = 0;
            let visited = self
                .table
                .read_documents()
                .for_each_next_shared_fields_borrowed(
                    after,
                    self.shared_page_rows,
                    &fields,
                    &mut |id, values| {
                        last = Some(id);
                        let result = (|| -> ExecResult<()> {
                            // Ordinary shared pages validate their layout only after finding a row, before its predicate.
                            if size.is_none() {
                                size = Some(self.batch_layout.for_schema(&self.schema)?);
                            }
                            let size = size.as_mut().expect("nonempty shared page");
                            if let Some(predicate) = &self.predicate {
                                if !predicate.keep(values)? {
                                    return Ok(());
                                }
                            }
                            let before = size.bytes();
                            size.append_borrowed(values)?;
                            largest_row = largest_row.max(size.bytes() - before);
                            sink.append(values)
                        })();
                        if let Err(error) = result {
                            failure = Some(error);
                            return false;
                        }
                        largest_row == 0
                            || size.as_ref().expect("nonempty shared page").bytes()
                                < self.scan_batch_bytes
                    },
                );
            if let Some(error) = failure {
                return Err(error);
            }
            let visited = visited.ok_or_else(|| {
                crate::ExecError::Other("stable borrowed projection became unavailable".into())
            })?;
            if let Some(last) = last {
                after = Some(last);
                self.input = ScoredInputCursor::All { after };
            }
            if visited == 0 {
                return Ok(true);
            }
            if largest_row != 0 {
                self.shared_page_rows = self
                    .scan_batch_bytes
                    .div_ceil(largest_row)
                    .clamp(1, crate::DEFAULT_BATCH_SIZE);
            }
        }
    }
}
