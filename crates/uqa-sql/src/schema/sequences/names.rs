//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve persisted sequence names against the loaded catalog, independently of `search_path`.
use uqa_core::RelationIdentity;
pub trait StoredSequenceRegistry {
    fn contains_sequence(&self, relation: &RelationIdentity) -> bool;
    fn sequence_names(&self) -> Vec<RelationIdentity>;
}
pub fn resolve_stored_sequence_reference(
    catalog: &dyn StoredSequenceRegistry,
    reference: &str,
) -> Result<String, String> {
    let (schema, local_name) = RelationIdentity::parse_reference(reference)
        .map_err(|error| format!("invalid persisted sequence reference `{reference}`: {error}"))?;
    if let Some(schema) = schema {
        let target = RelationIdentity::new(schema, local_name);
        if catalog.contains_sequence(&target) {
            return Ok(target.qualified_name());
        }
        return Err(format!(
            "dangling persisted sequence reference `{reference}`"
        ));
    }
    let candidates = catalog
        .sequence_names()
        .into_iter()
        .filter(|candidate| candidate.name == local_name)
        .map(|candidate| candidate.qualified_name())
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err(format!(
            "dangling persisted sequence reference `{reference}`"
        )),
        _ => Err(format!(
            "ambiguous persisted sequence reference `{reference}` matches {}",
            candidates.join(", ")
        )),
    }
}
