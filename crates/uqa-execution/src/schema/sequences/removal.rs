//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The services that remove a sequence's own state, check its owner, and forget what records it as a source of values.
use super::dependency_lifecycle::SequenceDependencyContext;
use uqa_sql::catalog::security::sequence_inquiry::SequencePrivilegeInquiry;
pub trait SequenceRemovalPublication {
    fn remove_state(&self, name: &str) -> Result<bool, String>;
}
/// Borrow the removal services lazily, so a context that contains them does not construct them eagerly.
pub trait SequenceRemovalInputs {
    fn sequence_removal_context(&self) -> SequenceRemovalContext<'_>;
}
pub struct SequenceRemovalContext<'a> {
    pub publication: &'a dyn SequenceRemovalPublication,
    pub privileges: SequencePrivilegeInquiry<'a>,
    pub dependencies: SequenceDependencyContext<'a>,
}
