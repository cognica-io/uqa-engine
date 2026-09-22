//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column-incarnation mapping distinguishes retained base rows from current private rows.

use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::StoredDocument;

pub(super) struct RowLayout<'a> {
    columns: &'a [ColumnDef],
    source: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> RowLayout<'a> {
    pub(super) fn new(source: &'a [ColumnDef], columns: &'a [ColumnDef]) -> Self {
        let by_id = columns
            .iter()
            .filter_map(|column| column.object_id.map(|id| (id, column)))
            .collect::<BTreeMap<_, _>>();
        let source = source
            .iter()
            .map(|column| {
                let target = column
                    .object_id
                    .and_then(|id| by_id.get(&id).copied())
                    .or_else(|| {
                        columns.iter().find(|target| {
                            target.name == column.name
                                && (column.object_id.is_none() || target.object_id.is_none())
                        })
                    });
                (
                    column.name.as_str(),
                    target.map(|target| target.name.as_str()),
                )
            })
            .collect();
        Self { columns, source }
    }

    pub(super) fn adapt_base(
        &self,
        mut document: StoredDocument,
    ) -> Result<StoredDocument, SQLError> {
        // Remove every source slot before installing targets so rename chains and name reuse cannot overwrite another column incarnation.
        let fields = document.fields_mut();
        let moved = self
            .source
            .iter()
            .filter_map(|(source, target)| {
                let value = fields.remove(*source);
                target.and_then(|target| value.map(|value| (target, value)))
            })
            .collect::<Vec<_>>();
        for (target, value) in moved {
            fields.insert(target.to_string(), value);
        }
        self.complete_private(document)
    }

    pub(super) fn complete_private(
        &self,
        mut document: StoredDocument,
    ) -> Result<StoredDocument, SQLError> {
        let fields = document.fields_mut();
        for column in self.columns {
            if column.generated.is_none() && !fields.contains_key(&column.name) {
                fields.insert(
                    column.name.clone(),
                    column.missing_value.clone().unwrap_or(Value::Null),
                );
            }
        }
        crate::query::generated::materialize_missing_generated_columns(self.columns, fields)?;
        Ok(document)
    }
}
