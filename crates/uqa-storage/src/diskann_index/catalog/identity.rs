//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL owners resolve immutable index identities from the provider's captured definition.

use super::invalid;
use crate::key_value::KeyValueReadRevision;
use crate::mvcc::DatabaseId;
use crate::{read_control::StorageReadControl, StorageBackendResult};

/// Interpret the captured SQL definition in its owning crate. Storage supplies the actual retained definition and table incarnation; a resolver must validate their relationship before returning the immutable index object identity. Resolution borrows provider-owned bytes and must not reenter storage.
pub trait DiskANNIndexResolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]>;
}

/// Catalog incarnations selected by a retained canonical source. Physical handles are allocated from these full identities, never from truncated IDs or SQL names. This scope does not authorize generation publication without its source's current-definition guard.
pub struct DiskANNIndexScope {
    pub(crate) table: [u8; 16],
    pub(crate) storage: [u8; 16],
    pub(crate) index: [u8; 16],
    revision: KeyValueReadRevision,
    source: [StorageReadControl; 2],
    control: StorageReadControl,
}

impl DiskANNIndexScope {
    pub(crate) fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        for source in &self.source {
            source.check()?;
        }
        self.control.check()?;
        control.check()
    }
    pub fn table_object(&self) -> [u8; 16] {
        self.table
    }

    pub fn storage_generation(&self) -> [u8; 16] {
        self.storage
    }

    pub fn index_object(&self) -> [u8; 16] {
        self.index
    }

    pub(crate) fn check(
        &self,
        database: DatabaseId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        self.check_control(control)?;
        if self.revision.record_database() != Some(database) {
            return Err(invalid(
                "physical session belongs to another transaction history",
            ));
        }
        Ok(())
    }
}

/// Provider adapter for an already captured catalog record. The supplied revision, owner and definition must all come from that same retained view; SQL expression decoding remains in the resolver's owner.
pub fn resolve_scope(
    resolver: &dyn DiskANNIndexResolver,
    owner: ([u8; 16], [u8; 16]),
    definition: Option<&str>,
    revision: &KeyValueReadRevision,
    source: &StorageReadControl,
    capture: &StorageReadControl,
    control: &StorageReadControl,
) -> StorageBackendResult<DiskANNIndexScope> {
    source.check()?;
    capture.check()?;
    control.check()?;
    if owner.0 == [0; 16] || owner.1 == [0; 16] || revision.record_database().is_none() {
        return Err(invalid("catalog owner has no versioned incarnation"));
    }
    let definition = definition.ok_or_else(|| invalid("index has no stored SQL identity"))?;
    let index = resolver.resolve(definition, owner.0, control)?;
    if index == [0; 16] {
        return Err(invalid("resolved index incarnation is zero"));
    }
    source.check()?;
    capture.check()?;
    control.check()?;
    Ok(DiskANNIndexScope {
        table: owner.0,
        storage: owner.1,
        index,
        revision: revision.clone(),
        source: [source.clone(), capture.clone()],
        control: control.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnreachableResolver;
    impl DiskANNIndexResolver for UnreachableResolver {
        fn resolve(
            &self,
            _: &str,
            _: [u8; 16],
            _: &StorageReadControl,
        ) -> StorageBackendResult<[u8; 16]> {
            panic!("unproven catalog records must not reach SQL resolution")
        }
    }

    #[test]
    fn diskann_catalog_scope_rejects_whole_view_and_unversioned_provenance() {
        let control = StorageReadControl::with_limit(1024);
        for revision in [
            KeyValueReadRevision::fresh(),
            KeyValueReadRevision::records(
                DatabaseId::from_bytes([1; 16]),
                crate::mvcc::CommitSequence::INITIAL,
                None,
            ),
        ] {
            assert!(resolve_scope(
                &UnreachableResolver,
                ([2; 16], [3; 16]),
                Some("{}"),
                &revision,
                &control,
                &control,
                &control
            )
            .is_err());
        }
    }
}
