//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode one pending view update from the common bounded rule-image owner.

use super::{
    dml_join_rows, target_row, BTreeSet, OwnedPhysicalRow, SQLError, UpdatePlan, Value,
    ViewDmlTarget,
};
use crate::mutation::rules::RuleRowImage;
use uqa_storage::document_store::Document;

pub(super) struct PendingViewUpdate {
    pub old: Vec<Value>,
    pub new: Vec<Value>,
    pub source_context: Option<OwnedPhysicalRow>,
    pub evaluation_row: OwnedPhysicalRow,
    pub evaluated_assignments: BTreeSet<usize>,
}

impl PendingViewUpdate {
    pub(super) fn from_image(
        target: &ViewDmlTarget,
        statement: &UpdatePlan,
        image: RuleRowImage,
        condition_columns: Option<&BTreeSet<String>>,
    ) -> Result<Self, SQLError> {
        let old = view_values(target, image.old)?;
        let new = view_values(target, image.new)?;
        let physical = target_row(target, &statement.target_qualifier, &old)?;
        let evaluation_row = image.context.as_ref().map_or_else(
            || physical.clone(),
            |source| dml_join_rows(&physical, source),
        );
        // The condition pass evaluates exactly this statement-wide assignment subset. Replaying its stored values must never invoke those assignments again.
        let evaluated_assignments = statement
            .assignments
            .iter()
            .enumerate()
            .filter(|(_, assignment)| {
                condition_columns.is_none_or(|columns| {
                    assignment
                        .target
                        .targets()
                        .iter()
                        .any(|target| columns.contains(&target.column))
                })
            })
            .map(|(index, _)| index)
            .collect();
        Ok(Self {
            old,
            new,
            source_context: image.context,
            evaluation_row,
            evaluated_assignments,
        })
    }

    pub(super) fn into_image(self, target: &ViewDmlTarget) -> Result<RuleRowImage, SQLError> {
        if self.old.len() != target.columns.len() || self.new.len() != target.columns.len() {
            return Err(SQLError::Internal(
                "view rule row does not match its declared row type".into(),
            ));
        }
        Ok(RuleRowImage {
            old_storage_table: None,
            old_doc_id: None,
            old: Some(target.columns.iter().cloned().zip(self.old).collect()),
            new_storage_table: None,
            new_doc_id: None,
            new: Some(target.columns.iter().cloned().zip(self.new).collect()),
            context: self.source_context,
        })
    }
}

pub(super) fn view_values(
    target: &ViewDmlTarget,
    document: Option<Document>,
) -> Result<Vec<Value>, SQLError> {
    let mut document =
        document.ok_or_else(|| SQLError::Internal("pending view row lost its image".into()))?;
    target
        .columns
        .iter()
        .map(|column| {
            document.remove(column).ok_or_else(|| {
                SQLError::Internal(format!("pending view row lost column `{column}`"))
            })
        })
        .collect()
}
