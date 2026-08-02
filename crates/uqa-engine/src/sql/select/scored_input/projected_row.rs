//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed row view over a document-store projection.

use uqa_sql::expr::RowLookup;

use super::{
    SQLError, ScoredRowMetadata, Value, DOC_ID_COLUMN, SCORE_COLUMN, SCORE_PROVENANCE_COLUMN,
};

pub(super) enum ProjectedValueSlot {
    Field(usize),
    DocId,
    Score,
    ScoreProvenance,
    Missing,
}

impl ProjectedValueSlot {
    pub(super) fn compile(schema: &[String], fields: &[String]) -> Vec<Self> {
        schema
            .iter()
            .map(|column| match column.as_str() {
                DOC_ID_COLUMN => Self::DocId,
                SCORE_COLUMN => Self::Score,
                SCORE_PROVENANCE_COLUMN => Self::ScoreProvenance,
                _ => fields
                    .iter()
                    .position(|field| field == column)
                    .map_or(Self::Missing, Self::Field),
            })
            .collect()
    }
}

pub(super) struct ProjectedDocumentRow<'schema, 'row> {
    fields: &'schema [String],
    slots: &'schema [ProjectedValueSlot],
    values: &'row [&'row Value],
    doc_id: Option<Value>,
    score: Option<Value>,
    score_provenance: Option<Value>,
}

impl<'schema, 'row> ProjectedDocumentRow<'schema, 'row> {
    pub(super) fn new(
        fields: &'schema [String],
        slots: &'schema [ProjectedValueSlot],
        values: &'row [&'row Value],
        metadata: ScoredRowMetadata,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            fields,
            slots,
            values,
            doc_id: metadata.doc_id()?,
            score: metadata.score(),
            score_provenance: metadata.score_provenance(),
        })
    }

    fn value(&self, name: &str) -> Option<&Value> {
        match name {
            DOC_ID_COLUMN => self.doc_id.as_ref(),
            SCORE_COLUMN => self.score.as_ref(),
            SCORE_PROVENANCE_COLUMN => self.score_provenance.as_ref(),
            _ => self
                .fields
                .iter()
                .position(|field| field == name)
                .and_then(|index| self.values.get(index).copied()),
        }
    }
}

impl RowLookup for ProjectedDocumentRow<'_, '_> {
    fn column(&self, name: &str) -> Option<&Value> {
        self.value(name)
    }

    fn qualified_column(&self, _qualifier: &str, column: &str, key: &str) -> Option<&Value> {
        if key.is_empty() {
            self.value(column)
        } else {
            self.value(key).or_else(|| self.value(column))
        }
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        match self.slots.get(index)? {
            ProjectedValueSlot::Field(index) => self.values.get(*index).copied(),
            ProjectedValueSlot::DocId => self.doc_id.as_ref(),
            ProjectedValueSlot::Score => self.score.as_ref(),
            ProjectedValueSlot::ScoreProvenance => self.score_provenance.as_ref(),
            ProjectedValueSlot::Missing => None,
        }
    }
}
