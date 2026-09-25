//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Journal seeks and canonical validation use the same fixed committed/private source.

use super::{append, invalid, Record, RetainedDiskANNCanonical, BYTES};
use crate::diskann_index::{format::DiskANNChangeIdentity, DiskANNCanonicalRead};
use crate::read_control::StorageReadControl;
use crate::StorageBackendResult;
use uqa_core::DocId;

impl RetainedDiskANNCanonical {
    /// Return each currently journaled document once, including an empty replacement. Obsolete origins are skipped without loading their values. This does not establish which changes a published generation covers.
    pub fn next_change_after(
        &self,
        mut after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DiskANNChangeIdentity>> {
        loop {
            self.check_control(control)?;
            if after == Some(DocId::MAX) {
                return Ok(None);
            }
            let Some(document) = self.next_journal_document(after, control)? else {
                self.check_control(control)?;
                return Ok(None);
            };
            after = Some(document);
            let Some(canonical) = self.record(document, control)? else {
                continue;
            };
            let identity = DiskANNChangeIdentity::new(document, canonical.version());
            if self.matches_change(identity, canonical, control)? {
                self.check_control(control)?;
                return Ok(Some(identity));
            }
        }
    }

    fn next_journal_document(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        let cursor = after
            .map(|document| {
                append(
                    &self.changes,
                    &DiskANNChangeIdentity::document_end(document),
                    control,
                )
            })
            .transpose()?;
        let mut selected = None;
        let mut failure = None;
        let source =
            self.read
                .visit_keys_after(&self.changes, cursor.as_deref(), 1, control, &mut |key| {
                    if failure.is_none() {
                        let result = (|| {
                            self.check_control(control)?;
                            if selected.is_some() {
                                return Err(invalid("change cursor exceeded its requested page"));
                            }
                            let identity = DiskANNChangeIdentity::decode(
                                key.strip_prefix(&*self.changes)
                                    .ok_or_else(|| invalid("change key escaped its field"))?,
                            )?;
                            if after.is_some_and(|after| identity.document() <= after) {
                                return Err(invalid("change cursor did not advance"));
                            }
                            selected = Some(identity.document());
                            Ok(())
                        })();
                        if let Err(error) = result {
                            failure = Some(error);
                        }
                    }
                    if failure.is_some() {
                        Err(invalid("change key consumer rejected data"))
                    } else {
                        Ok(())
                    }
                });
        if let Some(error) = failure {
            return Err(error);
        }
        source?;
        self.check_control(control)?;
        Ok(selected)
    }

    fn matches_change(
        &self,
        identity: DiskANNChangeIdentity,
        canonical: Record,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        let key = append(&self.changes, &identity.encode(), control)?;
        let mut seen = false;
        let mut found = false;
        let mut failure = None;
        let source = self
            .read
            .visit_value_bounded(&key, BYTES, control, &mut |value| {
                if failure.is_none() {
                    let result = (|| {
                        self.check_control(control)?;
                        if seen {
                            return Err(invalid("change value was returned more than once"));
                        }
                        seen = true;
                        if let Some(value) = value {
                            if Record::decode(value, self.dimensions)? != canonical {
                                return Err(invalid(
                                    "change record differs from its canonical origin",
                                ));
                            }
                            found = true;
                        }
                        Ok(())
                    })();
                    if let Err(error) = result {
                        failure = Some(error);
                    }
                }
                if failure.is_some() {
                    Err(invalid("change value consumer rejected data"))
                } else {
                    Ok(())
                }
            });
        if let Some(error) = failure {
            return Err(error);
        }
        source?;
        if !seen {
            return Err(invalid("change value was not returned"));
        }
        self.check_control(control)?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests;
