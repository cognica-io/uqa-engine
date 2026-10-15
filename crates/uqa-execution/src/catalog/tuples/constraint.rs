//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constraint tuple fields retain their typed expression before public text projection.

use serde::{Deserialize, Serialize};
use uqa_core::Value;
use uqa_sql::{catalog::node_tree::Node, ResultRow};

/// A constraint's stored scalar/array fields and optional typed CHECK expression. Relation names and session-specific SQL reconstruction are not stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConstraintTuple {
    fields: ResultRow,
    expression: Option<Node>,
}

impl ConstraintTuple {
    pub(crate) fn new(fields: ResultRow, expression: Option<Node>) -> Self {
        Self { fields, expression }
    }

    /// Observe the stored `PostgreSQL` node tree directly, without consulting current routine names, descriptors, privileges or output settings.
    pub fn into_row(mut self) -> ResultRow {
        self.fields.insert(
            "conbin".into(),
            self.expression
                .map_or(Value::Null, |node| Value::Str(node.to_string())),
        );
        self.fields
    }

    pub fn expression(&self) -> Option<&Node> {
        self.expression.as_ref()
    }

    pub(crate) fn fields(&self) -> &ResultRow {
        &self.fields
    }

    pub(crate) fn read_selected(
        json: &str,
        include_expression: bool,
        select: impl FnOnce(&ResultRow) -> bool,
    ) -> uqa_storage::StorageBackendResult<Option<Self>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Borrowed<'a> {
            fields: ResultRow,
            #[serde(borrow)]
            expression: Option<&'a serde_json::value::RawValue>,
        }
        let Borrowed { fields, expression } = super::decode(json)?;
        if !select(&fields) {
            return Ok(None);
        }
        let expression = if include_expression {
            expression.map(|raw| super::decode(raw.get())).transpose()?
        } else {
            None
        };
        Ok(Some(Self { fields, expression }))
    }
}
