//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered INSERT candidates before and after default/identity evaluation.

use uqa_core::{CancellationToken, DocId, Value};
use uqa_sql::SQLError;
use uqa_storage::document_store::Document;

use crate::mutation::prepared::{decode_prepared_doc_id, encode_prepared_doc_id};
use crate::physical::physical_exec_error;
use crate::spill::BufferedIndexedSpill;
use crate::{PhysicalRow, RowSchema};

pub(super) struct PendingInsertRow {
    pub document: Document,
    pub identity: Option<(DocId, bool)>,
}

pub(super) struct PendingInsertRows {
    rows: BufferedIndexedSpill,
    cancellation: CancellationToken,
}

impl PendingInsertRows {
    pub fn new(bytes: usize, cancellation: CancellationToken) -> Self {
        Self {
            rows: BufferedIndexedSpill::new(RowSchema::new(vec![String::new(); 4]), bytes),
            cancellation,
        }
    }

    pub fn len(&self) -> usize {
        usize::try_from(self.rows.len()).expect("push checks the addressable row count")
    }

    pub fn push(
        &mut self,
        document: Document,
        identity: Option<(DocId, bool)>,
    ) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if self.rows.len() == usize::MAX as u64 {
            return Err(invalid(
                "candidate count exceeds the addressable index range",
            ));
        }
        let (id, supplied) = identity.map_or((Value::Null, Value::Null), |(id, supplied)| {
            (encode_prepared_doc_id(id), Value::Bool(supplied))
        });
        self.rows
            .push(&PhysicalRow::from_values(vec![
                Value::Int(1),
                Value::Map(document),
                id,
                supplied,
            ]))
            .map_err(physical_exec_error)
    }

    pub fn get(&mut self, index: usize) -> Result<PendingInsertRow, SQLError> {
        self.cancellation.check()?;
        if index >= self.len() {
            return Err(invalid("candidate index is outside the retained row set"));
        }
        let row = self.rows.get(index as u64).map_err(physical_exec_error)?;
        let [version, document, id, supplied]: [Value; 4] =
            row.into_physical_values()
                .try_into()
                .map_err(|_| invalid("candidate has an invalid width"))?;
        if version != Value::Int(1) {
            return Err(invalid("candidate has an invalid version"));
        }
        let Value::Map(document) = document else {
            return Err(invalid("candidate has no document"));
        };
        let identity = match (id, supplied) {
            (Value::Null, Value::Null) => None,
            (id, Value::Bool(supplied)) => Some((
                decode_prepared_doc_id(id, "INSERT candidate spill")?,
                supplied,
            )),
            _ => return Err(invalid("candidate has an invalid prepared identity")),
        };
        Ok(PendingInsertRow { document, identity })
    }
}

fn invalid(message: &str) -> SQLError {
    SQLError::Internal(format!("INSERT candidate spill: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_candidates_preserve_full_width_identities_with_bounded_payload_retention() {
        let allocation = allocation_counter::measure(|| {
            let mut rows = PendingInsertRows::new(64 * 1024, CancellationToken::new());
            for id in 0..512 {
                let document = [
                    ("id".into(), Value::Int(id)),
                    ("body".into(), Value::Str("x".repeat(32768))),
                ]
                .into_iter()
                .collect();
                let identity = (id % 3 != 0).then_some((DocId::MAX - id as DocId, id % 2 == 0));
                rows.push(document, identity).unwrap();
            }
            assert_eq!(rows.len(), 512);
            for _ in 0..2 {
                for id in 0..512 {
                    let row = rows.get(id).unwrap();
                    assert_eq!(row.document.get("id"), Some(&Value::Int(id as i64)));
                    let Some(Value::Str(body)) = row.document.get("body") else {
                        panic!("text body")
                    };
                    assert_eq!(body.len(), 32768);
                    assert!(body.as_bytes().iter().all(|b| *b == b'x'));
                    assert_eq!(
                        row.identity,
                        (id % 3 != 0).then_some((DocId::MAX - id as DocId, id % 2 == 0))
                    );
                }
            }
            assert!(rows.get(512).is_err());
        });
        assert!(allocation.bytes_max < 2 * 1024 * 1024, "{allocation:?}");
    }

    #[test]
    fn insert_candidates_keep_cancellation_and_reject_invalid_identity_records() {
        let token = CancellationToken::new();
        let mut rows = PendingInsertRows::new(64, token.clone());
        rows.push(Document::new(), None).unwrap();
        assert!(rows.get(0).unwrap().document.is_empty());
        rows.rows
            .push(&PhysicalRow::from_values(vec![
                Value::Int(1),
                Value::Map(Document::new()),
                Value::Null,
                Value::Bool(true),
            ]))
            .unwrap();
        assert!(rows.get(1).is_err());
        token.cancel();
        assert_eq!(rows.get(0).err().unwrap().sqlstate(), Some("57014"));
        assert_eq!(
            rows.push(Document::new(), None).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(rows.len(), 2);
    }
}
