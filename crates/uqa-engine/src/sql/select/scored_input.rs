//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Streaming scored-document input adapters.

mod hierarchy;
mod materialize;

pub(in crate::sql) use hierarchy::HierarchyScoredDocumentSource;

use super::{
    doc_id_value, Arc, DocId, ExecResult, RecheckDoc, ResultRow, SQLError, ScoredEntry, Value,
    DOC_ID_COLUMN, SCORE_COLUMN, TABLE_OID_COLUMN, XMIN_COLUMN,
};
use crate::sql::from_rows::RelationMetadataProjection;
use crate::sql::{META_DOC_ID_COLUMN, META_QUALIFIER, META_SCORE_COLUMN};

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
    TableOid,
}

impl HiddenColumn {
    const ALL: [Self; 3] = [Self::DocId, Self::Score, Self::TableOid];

    fn bit(self) -> u8 {
        match self {
            Self::DocId => 1,
            Self::Score => 2,
            Self::TableOid => 4,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::DocId => DOC_ID_COLUMN,
            Self::Score => SCORE_COLUMN,
            Self::TableOid => TABLE_OID_COLUMN,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct HiddenColumns(u8);

impl HiddenColumns {
    fn from_schema(
        schema: &[String],
        definitions: &[uqa_sql::ast::ColumnDef],
        score_origin: ScoreOrigin,
    ) -> Self {
        let mut columns = Self(0);
        for column in HiddenColumn::ALL {
            let user_column = definitions
                .iter()
                .any(|definition| definition.name == column.name());
            if !user_column && schema.iter().any(|candidate| candidate == column.name()) {
                columns.insert(column);
            }
        }
        if score_origin.is_retrieval()
            && !definitions
                .iter()
                .any(|definition| definition.name == SCORE_COLUMN)
        {
            columns.insert(HiddenColumn::Score);
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
    table_oid: Option<i64>,
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

    pub(super) fn table_oid(self) -> Option<Value> {
        self.columns
            .contains(HiddenColumn::TableOid)
            .then(|| self.table_oid.map_or(Value::Null, Value::Int))
    }

    pub(super) fn insert_into(self, row: &mut ResultRow) -> Result<(), SQLError> {
        if let Some(value) = self.doc_id()? {
            row.insert(DOC_ID_COLUMN.into(), value);
        }
        if let Some(value) = self.score() {
            row.insert(SCORE_COLUMN.into(), value);
        }
        if let Some(value) = self.table_oid() {
            row.insert(TABLE_OID_COLUMN.into(), value);
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

    /// Discard rows whose score cannot reach a score-first SQL slice while
    /// retaining the complete boundary-score group for exact secondary
    /// ordering. The relational sort still chooses the final `LIMIT` rows.
    pub(in crate::sql) fn retain_top_scores_with_ties(&mut self, top_k: usize) {
        let Self::Entries {
            entries,
            score_bearing: true,
        } = self
        else {
            return;
        };
        if top_k == 0 {
            entries.clear();
            return;
        }
        if entries.len() <= top_k {
            return;
        }
        let (_, boundary, _) =
            entries.select_nth_unstable_by(top_k - 1, |a, b| b.score.total_cmp(&a.score));
        let cutoff = boundary.score;
        entries.retain(|entry| !entry.score.total_cmp(&cutoff).is_lt());
    }
}

pub(in crate::sql) struct ScoredDocumentSource {
    table_name: String,
    table: Arc<crate::TableState>,
    column_definitions: Vec<uqa_sql::ast::ColumnDef>,
    input: ScoredInputCursor,
    schema: uqa_execution::RowSchema,
    projected_fields: Vec<String>,
    projected_slots: Vec<uqa_execution::ProjectedValueSlot>,
    predicate: Option<uqa_execution::ProjectedPredicate>,
    hidden_columns: HiddenColumns,
    metadata_doc_id_attribute: Option<uqa_sql::ast::InternalColumnRef>,
    metadata_score_attribute: Option<uqa_sql::ast::InternalColumnRef>,
    appended_doc_id_attribute: Option<uqa_sql::ast::InternalColumnRef>,
    appended_score_attribute: Option<uqa_sql::ast::InternalColumnRef>,
    table_oid: Option<i64>,
    ordering: Vec<uqa_execution::PhysicalOrder>,
    input_guarantees_presence: bool,
    lock_origin: Option<(Arc<str>, Arc<str>)>,
    recheck_pinned: bool,
    recheck_documents: std::collections::BTreeMap<DocId, Arc<uqa_storage::StoredDocument>>,
}

pub(in crate::sql) enum ScoredInputCursor {
    All { after: Option<DocId> },
    Entries(std::vec::IntoIter<ScoredEntry>),
}

#[derive(Clone, Copy, Default)]
pub(in crate::sql) struct ScoredSourceAttributes {
    shared_score: Option<uqa_sql::ast::InternalColumnRef>,
    metadata: RelationMetadataProjection,
}

impl ScoredSourceAttributes {
    pub(in crate::sql) fn shared_score(
        column: uqa_sql::ast::InternalColumnRef,
        metadata: RelationMetadataProjection,
    ) -> Self {
        Self {
            shared_score: Some(column),
            metadata,
        }
    }
}

fn expose_metadata_namespace(
    schema: &uqa_execution::RowSchema,
    doc_id: Option<uqa_sql::ast::InternalColumnRef>,
    score: Option<uqa_sql::ast::InternalColumnRef>,
) -> uqa_execution::RowSchema {
    let aliases = [
        doc_id.and_then(|column| {
            schema.internal_slot(column).map(|slot| {
                (
                    uqa_execution::ColumnIdentity::qualified(META_QUALIFIER, META_DOC_ID_COLUMN),
                    slot,
                    Some(uqa_sql::ast::ColumnType::BigInteger),
                )
            })
        }),
        score.and_then(|column| {
            schema.internal_slot(column).map(|slot| {
                (
                    uqa_execution::ColumnIdentity::qualified(META_QUALIFIER, META_SCORE_COLUMN),
                    slot,
                    Some(uqa_sql::ast::ColumnType::DoublePrecision),
                )
            })
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if aliases.is_empty() {
        schema.clone()
    } else {
        uqa_execution::RowSchema::with_physical_identity_aliases(schema, &aliases)
    }
}

fn bind_structural_metadata_attribute(
    schema: &uqa_execution::RowSchema,
    hidden_columns: HiddenColumns,
    hidden: HiddenColumn,
    column: uqa_sql::ast::InternalColumnRef,
    ty: uqa_sql::ast::ColumnType,
) -> (uqa_execution::RowSchema, bool) {
    if hidden_columns.contains(hidden) {
        let position = schema
            .position(hidden.name())
            .expect("requested source metadata must have a logical position");
        let slot = schema
            .physical_slot(position)
            .expect("requested source metadata must have a physical slot");
        return (
            uqa_execution::RowSchema::with_physical_internal_aliases(
                schema,
                &[(column, slot, Some(ty))],
            ),
            false,
        );
    }
    (
        uqa_execution::RowSchema::append_internal_typed(schema, &[(column, Some(ty))]),
        true,
    )
}

impl ScoredDocumentSource {
    pub(in crate::sql) fn new(
        table_name: &str,
        table: Arc<crate::TableState>,
        input: ScoredInput,
        schema: Vec<String>,
        ordered_primary_key: Option<String>,
        predicate: Option<uqa_execution::ProjectedPredicate>,
    ) -> Self {
        Self::new_with_metadata(
            table_name,
            table,
            input,
            schema,
            ordered_primary_key,
            predicate,
            RelationMetadataProjection::default(),
        )
    }

    pub(in crate::sql) fn new_with_metadata(
        table_name: &str,
        table: Arc<crate::TableState>,
        input: ScoredInput,
        schema: Vec<String>,
        ordered_primary_key: Option<String>,
        predicate: Option<uqa_execution::ProjectedPredicate>,
        metadata: RelationMetadataProjection,
    ) -> Self {
        Self::new_configured(
            table_name,
            table,
            input,
            schema,
            ordered_primary_key,
            predicate,
            ScoredSourceAttributes {
                metadata,
                ..ScoredSourceAttributes::default()
            },
        )
    }

    /// Build one physical scored source while optionally sharing the opaque score attribute with sibling scans that form one logical relation.
    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    pub(in crate::sql) fn new_configured(
        table_name: &str,
        table: Arc<crate::TableState>,
        input: ScoredInput,
        mut schema: Vec<String>,
        ordered_primary_key: Option<String>,
        predicate: Option<uqa_execution::ProjectedPredicate>,
        attributes: ScoredSourceAttributes,
    ) -> Self {
        let ScoredSourceAttributes {
            shared_score: score_column,
            metadata,
        } = attributes;
        // Projection pruning may remove the primary-key column. An ordering
        // property is only meaningful when the ordered value is present in
        // the physical output schema.
        let ordered_primary_key = ordered_primary_key
            .and_then(|column| schema.iter().position(|candidate| candidate == &column));
        let (input, score_origin, ordering, input_guarantees_presence) = match input {
            ScoredInput::All => (
                ScoredInputCursor::All { after: None },
                ScoreOrigin::Unscored,
                ordered_primary_key
                    .into_iter()
                    .map(|position| uqa_execution::PhysicalOrder {
                        position,
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
                        .map(|position| uqa_execution::PhysicalOrder {
                            position,
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
        let column_definitions = table.columns.read().clone();
        let hidden_columns = HiddenColumns::from_schema(&schema, &column_definitions, score_origin);
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
                !HiddenColumn::ALL.iter().any(|hidden| {
                    hidden_columns.contains(*hidden) && column.as_str() == hidden.name()
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        let extra_columns = HiddenColumn::ALL.map(HiddenColumn::name);
        let projected_slots =
            uqa_execution::ProjectedValueSlot::compile(&schema, &projected_fields, &extra_columns);
        let column_types = schema
            .iter()
            .map(|column| {
                column_definitions
                    .iter()
                    .find(|definition| definition.name == *column)
                    .map(|definition| definition.ty.clone())
                    .or(match column.as_str() {
                        DOC_ID_COLUMN => Some(uqa_sql::ast::ColumnType::BigInteger),
                        SCORE_COLUMN => Some(uqa_sql::ast::ColumnType::DoublePrecision),
                        TABLE_OID_COLUMN => Some(uqa_sql::ast::ColumnType::Oid),
                        XMIN_COLUMN => Some(uqa_sql::ast::ColumnType::Xid),
                        _ => None,
                    })
            })
            .collect();
        let wildcard_hidden = schema
            .iter()
            .enumerate()
            .filter_map(|(position, name)| {
                (name == XMIN_COLUMN
                    || HiddenColumn::ALL.iter().any(|hidden| {
                        hidden_columns.contains(*hidden) && name.as_str() == hidden.name()
                    }))
                .then_some(position)
            })
            .collect::<Vec<_>>();
        let schema = uqa_execution::RowSchema::with_types(schema, column_types);
        let schema =
            uqa_execution::RowSchema::with_wildcard_hidden_positions(&schema, wildcard_hidden);
        let mut schema = schema;
        let mut metadata_doc_id_attribute = None;
        let mut metadata_score_attribute = None;
        let mut appended_doc_id_attribute = None;
        let mut appended_score_attribute = None;
        if metadata.includes_doc_id() {
            let column = uqa_sql::ast::InternalRelationId::allocate().column(0);
            metadata_doc_id_attribute = Some(column);
            let (bound, appended) = bind_structural_metadata_attribute(
                &schema,
                hidden_columns,
                HiddenColumn::DocId,
                column,
                uqa_sql::ast::ColumnType::BigInteger,
            );
            schema = bound;
            appended_doc_id_attribute = appended.then_some(column);
        }
        if score_origin.is_retrieval() || metadata.includes_score() {
            let column = score_column
                .unwrap_or_else(|| uqa_sql::ast::InternalRelationId::allocate().column(0));
            if metadata.includes_score() {
                metadata_score_attribute = Some(column);
            }
            let (bound, appended) = bind_structural_metadata_attribute(
                &schema,
                hidden_columns,
                HiddenColumn::Score,
                column,
                uqa_sql::ast::ColumnType::DoublePrecision,
            );
            schema = bound;
            appended_score_attribute = appended.then_some(column);
            if score_origin.is_retrieval() {
                schema = uqa_execution::RowSchema::with_score_source(&schema, None, column);
            }
        }
        schema =
            expose_metadata_namespace(&schema, metadata_doc_id_attribute, metadata_score_attribute);
        Self {
            table_name: table_name.to_string(),
            table,
            column_definitions,
            input,
            schema,
            projected_fields,
            projected_slots,
            predicate,
            hidden_columns,
            metadata_doc_id_attribute,
            metadata_score_attribute,
            appended_doc_id_attribute,
            appended_score_attribute,
            table_oid: None,
            ordering,
            input_guarantees_presence,
            lock_origin: None,
            recheck_pinned: false,
            recheck_documents: std::collections::BTreeMap::new(),
        }
    }

    pub(in crate::sql) fn with_lock_origin(mut self, origin: Option<(Arc<str>, Arc<str>)>) -> Self {
        self.lock_origin = origin;
        self
    }

    pub(in crate::sql) fn with_table_oid(mut self, table_oid: i64) -> Self {
        self.table_oid = Some(table_oid);
        self
    }

    /// Pin this scan to a tuple-local recheck candidate's tuples. A ranked input (retrieval predicate) was re-executed against the latest index state when the recheck rebuilt the scan, so a pinned tuple survives only if the retrieval still matches it, exactly like `PostgreSQL` re-evaluating the WHERE clause on the substituted tuple; its score is the re-executed retrieval score. A plain scan input keeps every pinned tuple and lets the residual predicates decide.
    pub(in crate::sql) fn with_recheck_pins(mut self, pins: Option<Arc<Vec<RecheckDoc>>>) -> Self {
        let Some(pins) = pins else {
            return self;
        };
        let ranked: Option<std::collections::BTreeMap<DocId, f64>> = match &mut self.input {
            ScoredInputCursor::Entries(entries) => Some(
                entries
                    .by_ref()
                    .map(|entry| (entry.doc_id, entry.score))
                    .collect(),
            ),
            ScoredInputCursor::All { .. } => None,
        };
        let entries = pins
            .iter()
            .filter_map(|pin| match ranked.as_ref() {
                Some(scores) => scores.get(&pin.doc_id).map(|score| ScoredEntry {
                    doc_id: pin.doc_id,
                    score: *score,
                }),
                None => Some(ScoredEntry {
                    doc_id: pin.doc_id,
                    score: 0.0,
                }),
            })
            .collect::<Vec<_>>();
        if !entries
            .windows(2)
            .all(|pair| pair[0].doc_id <= pair[1].doc_id)
        {
            self.ordering.clear();
        }
        self.recheck_documents = pins
            .iter()
            .filter_map(|pin| {
                pin.document
                    .as_ref()
                    .map(|document| (pin.doc_id, Arc::clone(document)))
            })
            .collect();
        self.recheck_pinned = true;
        self.input = ScoredInputCursor::Entries(entries.into_iter());
        self
    }

    /// Bind every visible column to its relation without changing public labels or copying values.
    pub(in crate::sql) fn with_qualifier(mut self, qualifier: &str) -> Self {
        self.schema = uqa_execution::RowSchema::with_relation_qualifier(&self.schema, qualifier);
        self.schema = expose_metadata_namespace(
            &self.schema,
            self.metadata_doc_id_attribute,
            self.metadata_score_attribute,
        );
        self
    }

    fn row_metadata(&self, doc_id: DocId, score: f64) -> ScoredRowMetadata {
        ScoredRowMetadata {
            doc_id,
            score,
            table_oid: self.table_oid,
            columns: self.hidden_columns,
        }
    }

    fn append_metadata_attributes(
        &self,
        row: uqa_execution::PhysicalRow,
        doc_id: DocId,
        score: f64,
    ) -> Result<uqa_execution::PhysicalRow, SQLError> {
        let mut values = Vec::with_capacity(2);
        if self.appended_doc_id_attribute.is_some() {
            values.push(doc_id_value(doc_id)?);
        }
        if self.appended_score_attribute.is_some() {
            values.push(Value::Float(score));
        }
        Ok(if values.is_empty() {
            row
        } else {
            row.append_values(values)
        })
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
        if let Some(rows) = self.next_shared_physical_batch(max_rows)? {
            return Ok(rows);
        }
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
        if self.appended_doc_id_attribute.is_some() || self.appended_score_attribute.is_some() {
            return Ok(false);
        }
        if !executor.supports_projected_rows() {
            return Ok(false);
        }
        let supports_storage_borrowed_rows = executor.supports_storage_borrowed_rows();
        loop {
            if supports_storage_borrowed_rows {
                if let Some(reached_end) =
                    self.aggregate_shared_batch(uqa_execution::batch::DEFAULT_BATCH_SIZE, executor)?
                {
                    if reached_end {
                        return Ok(true);
                    }
                    continue;
                }
            }
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
    fn score_cutoff_retains_the_complete_boundary_tie_group() {
        let mut input = ScoredInput::entries(
            vec![
                ScoredEntry {
                    doc_id: 4,
                    score: 0.25,
                },
                ScoredEntry {
                    doc_id: 2,
                    score: 0.75,
                },
                ScoredEntry {
                    doc_id: 1,
                    score: 1.0,
                },
                ScoredEntry {
                    doc_id: 3,
                    score: 0.75,
                },
            ],
            true,
        );

        input.retain_top_scores_with_ties(2);

        let ScoredInput::Entries { mut entries, .. } = input else {
            panic!("score-bearing input changed variants");
        };
        entries.sort_by_key(|entry| entry.doc_id);
        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.doc_id, entry.score))
                .collect::<Vec<_>>(),
            vec![(1, 1.0), (2, 0.75), (3, 0.75)]
        );
    }

    #[test]
    fn zero_score_cutoff_empties_score_bearing_entries_only() {
        let entries = vec![ScoredEntry {
            doc_id: 1,
            score: 1.0,
        }];
        let mut scored = ScoredInput::entries(entries.clone(), true);
        scored.retain_top_scores_with_ties(0);
        assert!(matches!(
            scored,
            ScoredInput::Entries { entries, .. } if entries.is_empty()
        ));

        let mut unscored = ScoredInput::entries(entries, false);
        unscored.retain_top_scores_with_ties(0);
        assert!(matches!(
            unscored,
            ScoredInput::Entries { entries, .. } if entries.len() == 1
        ));
    }

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
        assert_eq!(retained.output_ordering()[0].position, 0);
    }

    #[test]
    fn recheck_pins_clear_primary_key_ordering_when_their_order_is_not_ascending() {
        let engine = crate::Engine::new();
        engine
            .sql(
                "CREATE TABLE pinned_order (id BIGINT PRIMARY KEY, payload TEXT)",
                &[],
            )
            .unwrap();
        let table = engine.require_table("pinned_order").unwrap();
        let source = ScoredDocumentSource::new(
            "pinned_order",
            table,
            ScoredInput::All,
            vec!["id".into()],
            Some("id".into()),
            None,
        )
        .with_recheck_pins(Some(Arc::new(vec![
            RecheckDoc {
                doc_id: 2,
                document: None,
            },
            RecheckDoc {
                doc_id: 1,
                document: None,
            },
        ])));

        assert!(source.output_ordering().is_empty());
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
        .with_qualifier("a");
        let schema = source.physical_schema().unwrap().clone();
        let rows = source.next_physical_batch(16).unwrap();

        assert_eq!(schema.columns(), ["id"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fragment_count(), 1);
        assert!(rows[0].lock_origins().is_empty());
        assert_eq!(
            schema.view(&rows[0]).qualified_column("a", "id"),
            Some(&Value::Int(7))
        );
    }

    #[test]
    fn locking_physical_cursor_attaches_the_requested_origin() {
        let engine = crate::Engine::new();
        engine
            .sql("CREATE TABLE locking_source (id BIGINT PRIMARY KEY)", &[])
            .unwrap();
        engine
            .sql("INSERT INTO locking_source (id) VALUES (7)", &[])
            .unwrap();
        let table = engine.require_table("locking_source").unwrap();
        let mut source = ScoredDocumentSource::new(
            "locking_source",
            table,
            ScoredInput::All,
            vec!["id".into()],
            Some("id".into()),
            None,
        )
        .with_qualifier("l")
        .with_lock_origin(Some((Arc::from("l"), Arc::from("locking_source"))));

        let rows = source.next_physical_batch(16).unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].lock_origins().len(), 1);
        assert_eq!(rows[0].lock_origins()[0].qualifier.as_ref(), "l");
        assert_eq!(
            rows[0].lock_origins()[0].storage_name.as_ref(),
            "locking_source"
        );
        assert_eq!(rows[0].lock_origins()[0].doc_id, 7);
    }
}
