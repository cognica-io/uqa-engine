//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind sequence authorization and ACL publication to live state and catalog services.

use crate::Engine;
use uqa_core::RelationIdentity;
use uqa_execution::catalog::security::sequence_lifecycle::{
    SequencePrivilegeContext, SequencePrivilegePublication, SequenceSecurityWrite,
};
use uqa_sql::{
    ast::GrantSequenceStmt,
    catalog::{
        resolution::RelationResolution,
        security::{
            sequence_grants::SequenceGrantNamespace,
            sequence_inquiry::{
                SequencePrivilegeInquiry, SequencePrivilegeResolution, SequenceSecurityCatalog,
                SequenceSecurityRead,
            },
            SequenceSecurity,
        },
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

impl SequenceSecurityCatalog for Engine {
    fn security_read(&self) -> SequenceSecurityRead<'_> {
        Box::new(self.durable.sequence_security.read())
    }
}
impl SequencePrivilegeResolution for Engine {
    fn visible_relation_kind(&self, reference: &str) -> Result<RelationResolution, SQLError> {
        self.resolve_visible_relation_kind(reference)
    }
    fn sequence_privilege_oid(
        &self,
        oid: i64,
    ) -> Result<Option<(String, RelationIdentity)>, SQLError> {
        self.sequence_privilege_context()
            .resolve_sequence_privilege_oid(oid)
    }
}
impl SequenceGrantNamespace for Engine {
    fn temporary_schema_name(&self) -> String {
        self.temporary_schema_name()
    }
    fn temporary_namespace_allocated(&self) -> bool {
        self.temporary_namespace_allocated()
    }
    fn has_namespace(&self, name: &str) -> Result<bool, String> {
        self.has_namespace(name).map_err(|error| error.to_string())
    }
}
impl SequencePrivilegePublication for Engine {
    fn prepare_writer(&self) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer().map(|_| ())
    }
    fn refresh_catalog(&self) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()
    }
    fn security_write(&self) -> SequenceSecurityWrite<'_> {
        Box::new(self.durable.sequence_security.write())
    }
    fn catalog_changed(&self) {
        self.note_catalog_registry_changed();
    }
    fn notice(&self, level: &str, message: &str) {
        self.push_sql_notice(level, message);
    }
}
impl Engine {
    pub(crate) fn sequence_privilege_inquiry(&self) -> SequencePrivilegeInquiry<'_> {
        SequencePrivilegeInquiry {
            names: self,
            roles: self,
            security: self,
            resolution: self,
        }
    }
    pub(crate) fn sequence_privilege_context(&self) -> SequencePrivilegeContext<'_> {
        SequencePrivilegeContext {
            inquiry: self.sequence_privilege_inquiry(),
            sequences: self,
            namespaces: self,
            publication: self,
            catalog: self.catalog_execution(),
            storage: self.storage.catalog.as_deref(),
        }
    }
    pub(crate) fn grant_sequence_privileges(
        &self,
        statement: &GrantSequenceStmt,
    ) -> Result<(), SQLError> {
        self.sequence_privilege_context()
            .grant_sequence_privileges(statement)
    }
    pub(crate) fn persist_sequence_security(
        &self,
        name: &str,
        relation: &RelationIdentity,
        security: &SequenceSecurity,
    ) -> Result<(), SQLError> {
        self.sequence_privilege_context()
            .persist_sequence_security(name, relation, security)
    }
    pub(crate) fn ensure_sequence_nextval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.sequence_privilege_inquiry()
            .ensure_sequence_nextval_privilege(name, relation)
    }
    pub(crate) fn ensure_sequence_currval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.sequence_privilege_inquiry()
            .ensure_sequence_currval_privilege(name, relation)
    }
    pub(crate) fn ensure_sequence_setval_privilege(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<(), SQLError> {
        self.sequence_privilege_inquiry()
            .ensure_sequence_setval_privilege(name, relation)
    }
    pub(crate) fn ensure_sequence_owner(
        &self,
        name: &str,
        relation: &RelationIdentity,
    ) -> Result<String, SQLError> {
        self.sequence_privilege_inquiry()
            .ensure_sequence_owner(name, relation)
    }
}
