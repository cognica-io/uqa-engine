//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow shared command fields and batch consecutive reads from the same immutable source.

use super::{Change, DocId, DocumentChanges, StorageBackendResult, Value};

impl DocumentChanges {
    pub(super) fn visit_projection(
        &self,
        ids: &[DocId],
        fields: &[&str],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> StorageBackendResult<()> {
        let nulls = vec![&Value::Null; fields.len()];
        let mut staged = self.layer_reader(ids);
        let mut index = 0;
        while index < ids.len() {
            if let Some((end, source)) = self.batch_source_run(ids, index, &mut staged)? {
                let mut keep_going = true;
                // A provider may hold a whole requested page while it projects it, so a long run reads one bounded page at a time.
                for page in ids[index..end].chunks(crate::DEFAULT_BATCH_SIZE) {
                    source.for_each_fields_multi_ref_with_presence(
                        page,
                        fields,
                        &mut |id, present, values| {
                            keep_going = visitor(id, present, values);
                            keep_going
                        },
                    )?;
                    if !keep_going {
                        break;
                    }
                }
                if !keep_going {
                    break;
                }
                index = end;
            } else {
                let id = ids[index];
                let change = self.batch_change(&mut staged, id)?;
                let keep_going = match change.as_deref().and_then(Change::fields) {
                    Some(document) => {
                        let values = fields
                            .iter()
                            .map(|field| document.get(*field).unwrap_or(&Value::Null))
                            .collect::<Vec<_>>();
                        visitor(id, true, &values)
                    }
                    None => visitor(id, false, &nulls),
                };
                if !keep_going {
                    break;
                }
                index += 1;
            }
        }
        Ok(())
    }
}
