//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type and value contracts for executor-retained rule inputs.

use super::{
    bind_source_action, rule_row_metadata, runtime_rule_resolver, BindRuleAction,
    BoundSetOrientedAction, RuleColumnMetadata, RuleRowMetadata, RuleRowSource, RuleRowValues,
};
use crate::{
    ast::{FromClause, InternalRelationId, RetainedRowSource},
    RowSchema, SQLError,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use uqa_core::Value;

/// Describe OLD/NEW input once, materialize one admitted event at a time and bind one set-oriented action to the executor's retained rows.
pub struct RetainedRuleInput<'a> {
    columns: &'a BTreeMap<String, RuleColumnMetadata>,
    metadata: RuleRowMetadata,
    schema: RowSchema,
}

impl<'a> RetainedRuleInput<'a> {
    pub fn new(columns: &'a BTreeMap<String, RuleColumnMetadata>) -> Self {
        let metadata = rule_row_metadata(InternalRelationId::allocate(), columns);
        let schema = RowSchema::with_internal_relation_metadata(
            metadata.relation,
            metadata
                .types
                .iter()
                .cloned()
                .zip(metadata.collations.iter().copied()),
        );
        let mut ordered: Vec<_> = columns.iter().collect();
        ordered.sort_by_key(|(_, field)| field.position);
        let types: crate::schema::RecordFields = ordered
            .iter()
            .map(|(_, field)| Some(field.ty.clone()))
            .collect();
        let record = crate::schema::RecordMetadata {
            types: Some(types.clone()),
            descriptor: Some(Arc::new(crate::schema::RecordDescriptor {
                memory: crate::schema::RecordMemory::default(),
                types,
                names: Some(ordered.iter().map(|(name, _)| (*name).clone()).collect()),
                fields: ordered
                    .iter()
                    .map(|(_, field)| crate::schema::RecordFieldMetadata {
                        collation_oid: field.collation_oid.unwrap_or_else(|| {
                            u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                                &field.ty,
                            ))
                            .expect("type collation identity")
                        }),
                        sql_unknown: false,
                    })
                    .collect(),
            })),
        };
        let schema = schema.with_physical_record_metadata([
            (columns.len() * 2, record.clone()),
            (columns.len() * 2 + 1, record),
        ]);
        Self {
            columns,
            metadata,
            schema,
        }
    }

    pub fn relation(&self) -> InternalRelationId {
        self.metadata.relation
    }

    pub fn schema(&self) -> &RowSchema {
        &self.schema
    }

    /// The first slots replace typed constants; whole OLD/NEW records and the ordinal replace untyped constants.
    pub fn admitted_types(&self) -> Arc<[Option<String>]> {
        self.metadata
            .types
            .iter()
            .enumerate()
            .map(|(slot, ty)| {
                (slot < self.columns.len() * 2)
                    .then(|| ty.as_ref().expect("declared rule field").catalog_name())
            })
            .collect()
    }

    pub fn row_values(
        &self,
        row: &dyn RuleRowValues,
        row_index: usize,
    ) -> Result<Vec<Value>, SQLError> {
        let resolver = runtime_rule_resolver(row, self.columns);
        let mut values = Vec::with_capacity(self.metadata.types.len());
        for column in self.columns.keys() {
            for (record, doc_id) in [
                (row.old_row(), row.old_doc_id()),
                (row.new_row(), row.new_doc_id()),
            ] {
                crate::expr::check_statement_cancellation()?;
                let (_, value) = resolver.field(record, doc_id, column)?;
                values.push(value);
            }
        }
        values.push(resolver.record(row.old_row(), row.old_doc_id())?.value);
        values.push(resolver.record(row.new_row(), row.new_doc_id())?.value);
        values.push(Value::Int(i64::try_from(row_index).map_err(|_| {
            SQLError::Internal("rewrite rule event row index exceeds BIGINT".into())
        })?));
        Ok(values)
    }

    pub fn bind_action(
        self,
        source: RetainedRowSource,
        bind_action: &BindRuleAction<'_>,
    ) -> Result<BoundSetOrientedAction, SQLError> {
        if source.relation() != self.metadata.relation || source.schema() != &self.schema {
            return Err(SQLError::Internal(
                "rewrite-rule retained input lost its identity or type descriptor".into(),
            ));
        }
        bind_source_action(
            RuleRowSource {
                clause: FromClause::RetainedRows(source),
                metadata: self.metadata,
            },
            bind_action,
        )
    }
}
