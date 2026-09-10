//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Services for an atomic point-identified mutation.
use crate::{
    mutation::{
        assignment::MutationAssignmentContext,
        command_scope::{CommandScopeSource, MutationCommandState},
        constraints::ConstraintContext,
    },
    query::locking::context::RowLockContext,
};
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_sql::SQLError;
pub type RowUpdateVectors = BTreeMap<String, Vec<Vec<f32>>>;
pub type RowIndependentUpdateValues = (BTreeMap<String, Value>, RowUpdateVectors);
pub trait PointMutationStorage {
    fn find_doc_id_by_field(
        &self,
        table: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<DocId>, SQLError>;
    fn patch_document_fields_with_vector_values(
        &self,
        table: &str,
        doc_id: DocId,
        updates: &BTreeMap<String, Value>,
        vectors: &RowUpdateVectors,
    ) -> Result<bool, SQLError>;
}
#[derive(Clone)]
pub struct PointMutationContext<'a, S: Clone + 'static> {
    pub assignment: MutationAssignmentContext<'a, S>,
    pub constraints: ConstraintContext<'a>,
    pub locking: RowLockContext<'a, S>,
    pub scopes: &'a dyn CommandScopeSource<S>,
    pub transaction: &'a dyn MutationCommandState,
    pub storage: &'a dyn PointMutationStorage,
}
impl<S: Clone + 'static> Copy for PointMutationContext<'_, S> {}
