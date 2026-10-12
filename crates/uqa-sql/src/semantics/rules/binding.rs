//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OLD/NEW value binding for set-oriented rewrite-rule actions.

use crate::{
    ast::Expr,
    plpgsql::{ResolvedVariable, VariableResolver},
    ResultRow as Document, SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};

/// Logical OLD/NEW values used by rule action binding. Physical origins and source contexts stay in the executor.
pub trait RuleRowValues {
    fn old_row(&self) -> Option<&Document>;
    fn new_row(&self) -> Option<&Document>;
    fn old_doc_id(&self) -> Option<DocId>;
    fn new_doc_id(&self) -> Option<DocId>;
}

mod actions;
pub use actions::{bind_insert_values_action, bind_set_oriented_action, BoundSetOrientedAction};

pub struct RuleColumnMetadata {
    pub ty: crate::ast::ColumnType,
    pub collation_oid: Option<u32>,
    pub uses_document_id: bool,
    pub position: usize,
}

impl RuleColumnMetadata {
    /// A projected OLD/NEW field is still a column of the event relation. Its source expression cannot export explicit collation strength across that boundary.
    pub fn bind_input(&self, expression: crate::ScalarExpr) -> crate::ScalarExpr {
        if !crate::schema::composites::type_is_collatable(&self.ty) {
            return expression;
        }
        let oid = self.collation_oid.unwrap_or_else(|| {
            u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                &self.ty,
            ))
            .expect("type collation identity")
        });
        crate::ScalarExpr::Collated {
            expr: Box::new(expression),
            ty: self.ty.clone(),
            collation: crate::ir::ResultCollation::new(
                crate::catalog::collations::derivation::Derivation::implicit(i64::from(oid)),
                oid,
            ),
        }
    }
}

pub(super) struct RuntimeRuleResolver<'a> {
    pub(super) old: Option<&'a Document>,
    pub(super) new: Option<&'a Document>,
    pub(super) old_doc_id: Option<DocId>,
    pub(super) new_doc_id: Option<DocId>,
    pub(super) columns: &'a BTreeMap<String, RuleColumnMetadata>,
}

impl RuntimeRuleResolver<'_> {
    pub(super) fn field_expression(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
        column: &str,
    ) -> Result<Expr, SQLError> {
        let (metadata, value) = self.field(record, doc_id, column)?;
        Ok(Expr::TypedLiteral {
            value,
            ty: metadata.ty.catalog_name(),
            collation_oid: metadata.collation_oid,
            composite_source: None,
        })
    }

    pub(super) fn record_field(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
        column: &str,
    ) -> Result<ResolvedVariable, SQLError> {
        let (metadata, value) = self.field(record, doc_id, column)?;
        Ok(ResolvedVariable {
            value,
            declared_type: Some(metadata.ty.catalog_name()),
        })
    }

    fn field(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
        column: &str,
    ) -> Result<(&RuleColumnMetadata, Value), SQLError> {
        let metadata = self
            .columns
            .get(column)
            .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?;
        let value = if let Some(value) = record.and_then(|record| record.get(column).cloned()) {
            value
        } else if metadata.uses_document_id {
            doc_id
                .filter(|doc_id| crate::semantics::key_identity::is_key_document_id(*doc_id))
                .map(i64::try_from)
                .transpose()
                .map_err(|_| {
                    SQLError::TypeMismatch("document id exceeds PostgreSQL bigint".into())
                })?
                .map_or(Value::Null, Value::Int)
        } else {
            Value::Null
        };
        Ok((metadata, value))
    }

    pub(super) fn record(
        &self,
        record: Option<&Document>,
        doc_id: Option<DocId>,
    ) -> Result<ResolvedVariable, SQLError> {
        let mut columns = self.columns.iter().collect::<Vec<_>>();
        columns.sort_by_key(|(_, metadata)| metadata.position);
        let fields = columns
            .into_iter()
            .map(|(column, _)| {
                self.record_field(record, doc_id, column)
                    .map(|field| (column.clone(), field.value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedVariable::untyped(Value::Record(fields.into())))
    }
}

impl VariableResolver for RuntimeRuleResolver<'_> {
    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self
                .field_expression(self.old, self.old_doc_id, column)
                .map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self
                .field_expression(self.new, self.new_doc_id, column)
                .map(Some);
        }
        Ok(None)
    }

    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") {
            return self.record(self.old, self.old_doc_id).map(Some);
        }
        if name.eq_ignore_ascii_case("new") {
            return self.record(self.new, self.new_doc_id).map(Some);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self
                .record_field(self.old, self.old_doc_id, column)
                .map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self
                .record_field(self.new, self.new_doc_id, column)
                .map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .resolve_name(qualifier)?
            .map(|record| Expr::Literal(record.value)))
    }
}
