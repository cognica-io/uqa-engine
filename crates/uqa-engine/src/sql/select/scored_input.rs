//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming scored-document input adapters.

use super::{
    doc_id_value, Arc, DocId, ExecResult, ResultRow, SQLError, ScoredEntry, Value, DOC_ID_COLUMN,
    SCORE_COLUMN, SCORE_PROVENANCE_COLUMN,
};

pub(in crate::sql) enum ScoredInput {
    All,
    Entries {
        entries: Vec<ScoredEntry>,
        score_bearing: bool,
    },
}

impl ScoredInput {
    pub(in crate::sql) fn entries(entries: Vec<ScoredEntry>, score_bearing: bool) -> Self {
        Self::Entries {
            entries,
            score_bearing,
        }
    }
}

pub(in crate::sql) struct ScoredDocumentSource {
    table_name: String,
    table: Arc<crate::TableState>,
    input: ScoredInputCursor,
    schema: Vec<String>,
    score_bearing: bool,
}

pub(in crate::sql) enum ScoredInputCursor {
    All { after: Option<DocId> },
    Entries(std::vec::IntoIter<ScoredEntry>),
}

impl ScoredDocumentSource {
    pub(in crate::sql) fn new(
        table_name: &str,
        table: Arc<crate::TableState>,
        input: ScoredInput,
        mut schema: Vec<String>,
    ) -> Self {
        for hidden in [DOC_ID_COLUMN, SCORE_COLUMN, SCORE_PROVENANCE_COLUMN] {
            if !schema.iter().any(|column| column == hidden) {
                schema.push(hidden.to_string());
            }
        }
        let (input, score_bearing) = match input {
            ScoredInput::All => (ScoredInputCursor::All { after: None }, false),
            ScoredInput::Entries {
                entries,
                score_bearing,
            } => (
                ScoredInputCursor::Entries(entries.into_iter()),
                score_bearing,
            ),
        };
        Self {
            table_name: table_name.to_string(),
            table,
            input,
            schema,
            score_bearing,
        }
    }

    fn next_entry(&mut self) -> Result<Option<ScoredEntry>, SQLError> {
        match &mut self.input {
            ScoredInputCursor::Entries(entries) => Ok(entries.next()),
            ScoredInputCursor::All { after } => {
                let next = self
                    .table
                    .document_store
                    .read()
                    .next_doc_id(*after)
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "scan document ids for `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                *after = next;
                Ok(next.map(|doc_id| ScoredEntry { doc_id, score: 0.0 }))
            }
        }
    }
}

impl uqa_execution::RowSource for ScoredDocumentSource {
    fn schema(&self) -> &[String] {
        &self.schema
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        let Some(entry) = self.next_entry()? else {
            return Ok(None);
        };
        let mut document = self
            .table
            .document_store
            .read()
            .get(entry.doc_id)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "read `{}` document {}: {error}",
                    self.table_name, entry.doc_id
                ))
            })?
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "access path returned document {}, but table `{}` omitted it",
                    entry.doc_id, self.table_name
                ))
            })?;
        document.insert(DOC_ID_COLUMN.into(), doc_id_value(entry.doc_id)?);
        document.insert(SCORE_COLUMN.into(), Value::Float(entry.score));
        document.insert(
            SCORE_PROVENANCE_COLUMN.into(),
            if self.score_bearing {
                Value::Float(entry.score)
            } else {
                Value::Null
            },
        );
        Ok(Some(document))
    }
}
