//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve persisted sequence names against the loaded catalog, independently of `search_path`.
use uqa_core::RelationIdentity;
mod production;
pub use production::resolve_stored_sequence_reference_with_control;
pub trait StoredSequenceRegistry {
    fn contains_sequence(&self, relation: &RelationIdentity) -> bool;
    fn sequence_names(&self) -> Vec<RelationIdentity>;
    fn visit_sequence_names_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
        visit: &mut dyn FnMut(&RelationIdentity) -> Result<(), crate::SQLError>,
    ) -> Result<(), crate::SQLError> {
        control.check()?;
        for name in self.sequence_names() {
            control.check()?;
            visit(&name)?;
        }
        Ok(())
    }
}
pub fn resolve_stored_sequence_reference(
    catalog: &dyn StoredSequenceRegistry,
    reference: &str,
) -> Result<String, String> {
    resolve_stored_sequence_reference_with_control(
        catalog,
        reference,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|name| {
        name.into_uncontrolled()
            .expect("ordinary sequence names have no planning lease")
    })
    .map_err(|error| match error {
        crate::SQLError::Internal(message) => message,
        error => error.to_string(),
    })
}
