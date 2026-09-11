//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow live sequence registries, session guards and physical storage inputs.
use super::super::{
    session::{
        NontransactionalSequenceValue, SessionLastSequenceReference, SessionSequenceCache,
        SessionSequenceValue,
    },
    SequenceState,
};
use crate::catalog::{
    sequence::restoration::SequencePersistenceRead,
    sequence_introspection::SequenceIntrospectionCatalog,
};
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::security::sequence_inquiry::SequencePrivilegeInquiry, SQLError};
use uqa_storage::{CatalogFacade, PersistentStorageSession, StorageBackendResult};
pub type SequenceStatesWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, SequenceState>> + 'a>;
pub type SequenceCachesWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, SessionSequenceCache>> + 'a>;
/// A retained read guard, including when the last-used identity and value are read together.
pub trait SequenceSessionRead {
    fn currvals(&self) -> &BTreeMap<RelationIdentity, SessionSequenceValue>;
    fn last(&self) -> Option<&SessionLastSequenceReference>;
}
pub trait SequenceSessionWrite {
    fn currvals_mut(&mut self) -> &mut BTreeMap<RelationIdentity, SessionSequenceValue>;
    fn last_mut(&mut self) -> &mut Option<SessionLastSequenceReference>;
}
pub trait SequenceValueRuntime {
    fn persistence(&self) -> SequencePersistenceRead<'_>;
    fn states_write(&self) -> SequenceStatesWrite<'_>;
    fn caches(&self) -> SequenceCachesWrite<'_>;
    fn session_read(&self) -> Box<dyn SequenceSessionRead + '_>;
    fn session_write(&self) -> Box<dyn SequenceSessionWrite + '_>;
    fn current_transaction_is_read_only(&self) -> bool;
    fn open_nontransactional_sequence_session(
        &self,
    ) -> StorageBackendResult<Option<PersistentStorageSession>>;
    fn prepare_explicit_transaction_writer(&self) -> Result<(), SQLError>;
    fn record_nontransactional_sequence_value(
        &self,
        definition_generation: [u8; 16],
        value: NontransactionalSequenceValue,
        defines_lastval: bool,
    );
}
pub struct SequenceValueContext<'a> {
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub privileges: SequencePrivilegeInquiry<'a>,
    pub runtime: &'a dyn SequenceValueRuntime,
    pub storage: Option<&'a dyn CatalogFacade>,
}
