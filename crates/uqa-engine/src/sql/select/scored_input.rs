//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming scored-document input adapters.

mod materialize;
mod projected_row;

use projected_row::ProjectedValueSlot;

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

#[derive(Clone, Copy)]
pub(super) enum ScoreOrigin {
    Unscored,
    Retrieval,
}

impl ScoreOrigin {
    fn from_score_bearing(score_bearing: bool) -> Self {
        if score_bearing {
            Self::Retrieval
        } else {
            Self::Unscored
        }
    }

    pub(super) fn is_retrieval(self) -> bool {
        matches!(self, Self::Retrieval)
    }
}

#[derive(Clone, Copy)]
pub(super) enum HiddenColumn {
    DocId,
    Score,
    ScoreProvenance,
}

impl HiddenColumn {
    const ALL: [Self; 3] = [Self::DocId, Self::Score, Self::ScoreProvenance];

    fn bit(self) -> u8 {
        match self {
            Self::DocId => 1,
            Self::Score => 2,
            Self::ScoreProvenance => 4,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::DocId => DOC_ID_COLUMN,
            Self::Score => SCORE_COLUMN,
            Self::ScoreProvenance => SCORE_PROVENANCE_COLUMN,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct HiddenColumns(u8);

impl HiddenColumns {
    fn from_schema(schema: &[String], score_origin: ScoreOrigin) -> Self {
        let mut columns = Self(0);
        for column in HiddenColumn::ALL {
            if schema.iter().any(|candidate| candidate == column.name()) {
                columns.insert(column);
            }
        }
        if score_origin.is_retrieval() {
            columns.insert(HiddenColumn::Score);
            columns.insert(HiddenColumn::ScoreProvenance);
        }
        columns
    }

    fn insert(&mut self, column: HiddenColumn) {
        self.0 |= column.bit();
    }

    pub(super) fn contains(self, column: HiddenColumn) -> bool {
        self.0 & column.bit() != 0
    }
}

#[derive(Clone, Copy)]
pub(super) struct ScoredRowMetadata {
    doc_id: DocId,
    score: f64,
    score_origin: ScoreOrigin,
    columns: HiddenColumns,
}

impl ScoredRowMetadata {
    pub(super) fn doc_id(self) -> Result<Option<Value>, SQLError> {
        self.columns
            .contains(HiddenColumn::DocId)
            .then(|| doc_id_value(self.doc_id))
            .transpose()
    }

    pub(super) fn score(self) -> Option<Value> {
        self.columns
            .contains(HiddenColumn::Score)
            .then_some(Value::Float(self.score))
    }

    pub(super) fn score_provenance(self) -> Option<Value> {
        self.columns
            .contains(HiddenColumn::ScoreProvenance)
            .then_some(if self.score_origin.is_retrieval() {
                Value::Float(self.score)
            } else {
                Value::Null
            })
    }

    pub(super) fn insert_into(self, row: &mut ResultRow) -> Result<(), SQLError> {
        if let Some(value) = self.doc_id()? {
            row.insert(DOC_ID_COLUMN.into(), value);
        }
        if let Some(value) = self.score() {
            row.insert(SCORE_COLUMN.into(), value);
        }
        if let Some(value) = self.score_provenance() {
            row.insert(SCORE_PROVENANCE_COLUMN.into(), value);
        }
        Ok(())
    }
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
    /// Relation qualifier to expose alongside each bare column name, set only
    /// when the enclosing block can evaluate a correlated subquery. See
    /// [`ScoredDocumentSource::with_outer_qualifier`].
    outer_qualifier: Option<String>,
    table: Arc<crate::TableState>,
    input: ScoredInputCursor,
    schema: uqa_execution::RowSchema,
    projected_fields: Vec<String>,
    projected_slots: Vec<ProjectedValueSlot>,
    predicate: Option<uqa_execution::ProjectedPredicate>,
    score_origin: ScoreOrigin,
    hidden_columns: HiddenColumns,
    ordering: Vec<uqa_execution::PhysicalOrder>,
    input_guarantees_presence: bool,
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
        ordered_primary_key: Option<String>,
        predicate: Option<uqa_execution::ProjectedPredicate>,
    ) -> Self {
        // Projection pruning may remove the primary-key column. An ordering
        // property is only meaningful when the ordered value is present in
        // the physical output schema.
        let ordered_primary_key =
            ordered_primary_key.filter(|column| schema.iter().any(|candidate| candidate == column));
        let (input, score_origin, ordering, input_guarantees_presence) = match input {
            ScoredInput::All => (
                ScoredInputCursor::All { after: None },
                ScoreOrigin::Unscored,
                ordered_primary_key
                    .into_iter()
                    .map(|column| uqa_execution::PhysicalOrder {
                        column,
                        descending: false,
                        nulls_first: None,
                        nullable: false,
                    })
                    .collect(),
                true,
            ),
            ScoredInput::Entries {
                entries,
                score_bearing,
            } => {
                let doc_ids_are_ordered = entries
                    .windows(2)
                    .all(|pair| pair[0].doc_id <= pair[1].doc_id);
                let ordering = if doc_ids_are_ordered {
                    ordered_primary_key
                        .into_iter()
                        .map(|column| uqa_execution::PhysicalOrder {
                            column,
                            descending: false,
                            nulls_first: None,
                            nullable: false,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    ScoredInputCursor::Entries(entries.into_iter()),
                    ScoreOrigin::from_score_bearing(score_bearing),
                    ordering,
                    false,
                )
            }
        };
        let hidden_columns = HiddenColumns::from_schema(&schema, score_origin);
        for column in HiddenColumn::ALL {
            if hidden_columns.contains(column)
                && !schema.iter().any(|candidate| candidate == column.name())
            {
                schema.push(column.name().to_string());
            }
        }
        let projected_fields = schema
            .iter()
            .filter(|column| {
                !matches!(
                    column.as_str(),
                    DOC_ID_COLUMN | SCORE_COLUMN | SCORE_PROVENANCE_COLUMN
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let projected_slots = ProjectedValueSlot::compile(&schema, &projected_fields);
        let schema = uqa_execution::RowSchema::new(schema);
        Self {
            table_name: table_name.to_string(),
            outer_qualifier: None,
            table,
            input,
            schema,
            projected_fields,
            projected_slots,
            predicate,
            score_origin,
            hidden_columns,
            ordering,
            input_guarantees_presence,
        }
    }

    /// Also emit `qualifier.column` for every projected column.
    ///
    /// A correlated subquery is evaluated against a row that merges the inner
    /// relation's columns over this one. Inner relations arrive already
    /// qualified (`allowed.id`), while a plain single-table scan emits bare
    /// names, so an outer reference such as `papers.id` had nothing to bind to:
    /// the merge overwrote the bare `id` with the inner value, and the
    /// qualified-lookup fallback then refused the bare name because another
    /// qualifier claimed it. The reference silently evaluated to NULL and the
    /// predicate matched nothing.
    ///
    /// Publishing the qualified name restores the symmetry. Bare lookups still
    /// hit the bare key first, so inner-scope shadowing is unchanged. This is
    /// opt-in because the extra keys cost row-construction time on a hot path,
    /// and only blocks containing a subquery can observe them.
    pub(in crate::sql) fn with_outer_qualifier(mut self, qualifier: Option<String>) -> Self {
        if let Some(qualifier) = qualifier.as_deref() {
            let aliases = self
                .projected_fields
                .iter()
                .map(|field| (format!("{qualifier}.{field}"), field.clone()))
                .collect::<Vec<_>>();
            self.schema = uqa_execution::RowSchema::with_lookup_aliases(&self.schema, &aliases);
        }
        self.outer_qualifier = qualifier;
        self
    }

    fn row_metadata(&self, doc_id: DocId, score: f64) -> ScoredRowMetadata {
        ScoredRowMetadata {
            doc_id,
            score,
            score_origin: self.score_origin,
            columns: self.hidden_columns,
        }
    }

    fn next_entries(&mut self, max_rows: usize) -> Result<Vec<ScoredEntry>, SQLError> {
        match &mut self.input {
            ScoredInputCursor::Entries(entries) => Ok(entries.by_ref().take(max_rows).collect()),
            ScoredInputCursor::All { after } => {
                let doc_ids = self
                    .table
                    .document_store
                    .read()
                    .next_doc_ids(*after, max_rows)
                    .map_err(|error| {
                        SQLError::Internal(format!(
                            "scan document ids for `{}`: {error}",
                            self.table_name
                        ))
                    })?;
                if let Some(doc_id) = doc_ids.last() {
                    *after = Some(*doc_id);
                }
                Ok(doc_ids
                    .into_iter()
                    .map(|doc_id| ScoredEntry { doc_id, score: 0.0 })
                    .collect())
            }
        }
    }
}

impl uqa_execution::RowSource for ScoredDocumentSource {
    fn schema(&self) -> &[String] {
        self.schema.columns()
    }

    fn physical_schema(&self) -> Option<&uqa_execution::RowSchema> {
        Some(&self.schema)
    }

    fn output_ordering(&self) -> &[uqa_execution::PhysicalOrder] {
        &self.ordering
    }

    fn next_row(&mut self) -> ExecResult<Option<ResultRow>> {
        Ok(self.next_batch(1)?.into_iter().next())
    }

    fn next_batch(&mut self, max_rows: usize) -> ExecResult<Vec<ResultRow>> {
        loop {
            let entries = self.next_entries(max_rows)?;
            if entries.is_empty() {
                return Ok(Vec::new());
            }
            let reached_end = entries.len() < max_rows;
            let rows = self.materialize_entries(&entries)?;
            if !rows.is_empty() || reached_end {
                return Ok(rows);
            }
        }
    }

    fn next_physical_batch(
        &mut self,
        max_rows: usize,
    ) -> ExecResult<Vec<uqa_execution::PhysicalRow>> {
        loop {
            let entries = self.next_entries(max_rows)?;
            if entries.is_empty() {
                return Ok(Vec::new());
            }
            let reached_end = entries.len() < max_rows;
            let rows = self.materialize_physical_entries(&entries)?;
            if !rows.is_empty() || reached_end {
                return Ok(rows);
            }
        }
    }

    fn consume_into_aggregate(
        &mut self,
        executor: &mut dyn uqa_execution::AggregateExecutor,
    ) -> ExecResult<bool> {
        if !executor.supports_projected_rows() {
            return Ok(false);
        }
        loop {
            let entries = self.next_entries(uqa_execution::batch::DEFAULT_BATCH_SIZE)?;
            if entries.is_empty() {
                return Ok(true);
            }
            let reached_end = entries.len() < uqa_execution::batch::DEFAULT_BATCH_SIZE;
            self.aggregate_entries(&entries, executor)?;
            if reached_end {
                return Ok(true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_execution::RowSource;
    use uqa_sql::expr::RowLookup;

    #[test]
    fn pruned_primary_key_is_not_advertised_as_output_ordering() {
        let engine = crate::Engine::new();
        engine
            .sql(
                "CREATE TABLE ordered_source (id BIGINT PRIMARY KEY, payload TEXT)",
                &[],
            )
            .unwrap();
        let table = engine.require_table("ordered_source").unwrap();

        let pruned = ScoredDocumentSource::new(
            "ordered_source",
            Arc::clone(&table),
            ScoredInput::All,
            vec!["payload".into()],
            Some("id".into()),
            None,
        );
        assert!(pruned.output_ordering().is_empty());

        let retained = ScoredDocumentSource::new(
            "ordered_source",
            table,
            ScoredInput::All,
            vec!["id".into(), "payload".into()],
            Some("id".into()),
            None,
        );
        assert_eq!(retained.output_ordering()[0].column, "id");
    }

    #[test]
    fn store_cursor_materializes_empty_projection_without_losing_rows() {
        let engine = crate::Engine::new();
        engine
            .sql(
                "CREATE TABLE presence_source (id BIGINT PRIMARY KEY, payload TEXT)",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "INSERT INTO presence_source (id, payload) VALUES (1, 'a'), (2, 'b')",
                &[],
            )
            .unwrap();
        let table = engine.require_table("presence_source").unwrap();
        let mut source = ScoredDocumentSource::new(
            "presence_source",
            table,
            ScoredInput::All,
            Vec::new(),
            None,
            None,
        );

        let rows = source.next_batch(16).unwrap();
        assert_eq!(rows, vec![ResultRow::new(), ResultRow::new()]);
    }

    #[test]
    fn physical_cursor_exposes_qualified_alias_without_copying_the_value() {
        let engine = crate::Engine::new();
        engine
            .sql("CREATE TABLE alias_source (id BIGINT PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO alias_source (id) VALUES (7)", &[])
            .unwrap();
        let table = engine.require_table("alias_source").unwrap();
        let mut source = ScoredDocumentSource::new(
            "alias_source",
            table,
            ScoredInput::All,
            vec!["id".into()],
            Some("id".into()),
            None,
        )
        .with_outer_qualifier(Some("a".into()));
        let schema = source.physical_schema().unwrap().clone();
        let rows = source.next_physical_batch(16).unwrap();

        assert_eq!(schema.columns(), ["id"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fragment_count(), 1);
        assert_eq!(
            schema.view(&rows[0]).qualified_column("a", "id", "a.id"),
            Some(&Value::Int(7))
        );
    }
}
