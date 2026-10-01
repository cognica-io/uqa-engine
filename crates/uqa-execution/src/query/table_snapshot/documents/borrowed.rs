//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pure projections preserve a provider's bounded storage borrow through aggregate input.

use super::{DocId, RetainedDocuments, StorageBackendError, StorageBackendResult, Value};

impl RetainedDocuments {
    pub(super) fn visit_borrowed_points(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        self.0.control.check()?;
        if self.0.changes.has_changes() {
            return Ok(None);
        }
        let Some(projection) = self.0.layout.projection(fields)? else {
            return Ok(None);
        };
        if projection.needs_presence() {
            return Ok(None);
        }
        let mut visited = 0;
        let mut more = true;
        let mut failure = None;
        let result = self.0.source.for_each_fields_multi_borrowed(
            ids,
            &projection.sources,
            &mut |id, present, values| {
                let projected = (|| {
                    self.0.control.check()?;
                    if !more
                        || ids.get(visited) != Some(&id)
                        || values.len() != projection.sources.len()
                    {
                        return Err(StorageBackendError::Other(
                            "borrowed point projection returned an invalid row".into(),
                        ));
                    }
                    projection.values(values, &[])
                })();
                match projected {
                    Ok(projected) => {
                        visited += 1;
                        more = visitor(id, present, &projected);
                        if let Err(error) = self.0.control.check() {
                            failure = Some(error);
                            return false;
                        }
                        more
                    }
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        let result = result?;
        if result.is_some_and(|count| count != visited || (more && count != ids.len()))
            || (result.is_none() && visited != 0)
        {
            return Err(StorageBackendError::Other(
                "borrowed point projection returned an invalid count".into(),
            ));
        }
        self.0.control.check()?;
        Ok(result)
    }

    pub(super) fn visit_borrowed_projection(
        &self,
        after: Option<DocId>,
        limit: usize,
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, &[&Value]) -> bool,
    ) -> StorageBackendResult<Option<usize>> {
        self.0.control.check()?;
        if self.0.changes.has_changes() {
            return Ok(None);
        }
        let Some(projection) = self.0.layout.projection(fields)? else {
            return Ok(None);
        };
        if projection.needs_presence() {
            return Ok(None);
        }
        let mut last = after;
        let mut visited = 0;
        let mut failure = None;
        let result = self.0.source.for_each_next_fields_borrowed(
            after,
            limit,
            &projection.sources,
            &mut |id, values| {
                let projected = (|| {
                    self.0.control.check()?;
                    if last.is_some_and(|last| id <= last)
                        || visited >= limit
                        || values.len() != projection.sources.len()
                    {
                        return Err(StorageBackendError::Other(
                            "borrowed document cursor returned an invalid row".into(),
                        ));
                    }
                    projection.values(values, &[])
                })();
                match projected {
                    Ok(projected) => {
                        last = Some(id);
                        visited += 1;
                        let more = visitor(id, &projected);
                        if let Err(error) = self.0.control.check() {
                            failure = Some(error);
                            return false;
                        }
                        more
                    }
                    Err(error) => {
                        failure = Some(error);
                        false
                    }
                }
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        let result = result?;
        if result.is_some_and(|count| count != visited) || (result.is_none() && visited != 0) {
            return Err(StorageBackendError::Other(
                "borrowed document cursor returned an invalid count".into(),
            ));
        }
        self.0.control.check()?;
        Ok(result)
    }
}
