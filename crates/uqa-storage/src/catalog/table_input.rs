//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical table definitions and independent ACL replacements retain one catalog view.

use super::{
    input::{
        CatalogInput, CatalogInputLimits, CatalogInputStep, CatalogKeyFilter, CatalogRowDecoder,
    },
    relation_acl::{decode_key_with_control, RelationAclTuple},
    RelationIdentity, TableSchema,
};
use crate::{
    key_value::KeyValueRead, read_control::StorageReadControl, StorageBackendError,
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::Budgeted;

/// One independently stored replacement, before catalog lookup and ordered application.
#[derive(Debug)]
pub struct CatalogTableAcl {
    pub relation: RelationIdentity,
    pub column: Option<String>,
    pub tuple: RelationAclTuple,
}

impl CatalogTableAcl {
    /// Preserve key validation before tuple validation and retain both original allocations.
    pub fn decode(
        key: &str,
        value: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        let key = decode_key_with_control(key, control)?;
        let tuple = RelationAclTuple::decode_with_control(value, control)?;
        control.check()?;
        let ((relation, column), mut memory) = key.into_parts();
        let (tuple, tuple_memory) = tuple.into_parts();
        memory.absorb(tuple_memory);
        Ok(Budgeted::new(
            Self {
                relation,
                column,
                tuple,
            },
            memory,
        ))
    }
}

#[derive(Debug)]
pub enum CatalogTableInputStep {
    Pending,
    Definition(Budgeted<Box<TableSchema>>),
    /// All definitions have been read. The catalog consumer performs its existing ordering and cross-record predicates before requesting ACL records.
    DefinitionsComplete,
    AclReplacement(Budgeted<CatalogTableAcl>),
    Complete,
}

/// Read original physical definitions followed by ACL replacements from one retained view. Returned definitions contain their inline authority; they are not an assembled catalog until the owner has applied the replacement records and its existing validation rules.
pub struct CatalogTableInput {
    state: State,
    control: StorageReadControl,
}

enum State {
    Reading(Budgeted<Box<Input>>),
    Complete,
    Failed,
}

struct Input {
    definitions: Option<CatalogInput<TableSchema>>,
    acls: CatalogInput<CatalogTableAcl>,
}

impl CatalogTableInput {
    pub(crate) fn control(&self) -> &StorageReadControl {
        &self.control
    }

    /// Both physical streams retain the supplied view before either reads a value. Prefixes must select exactly the provider's table definitions and independent ACL metadata.
    pub fn new(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        definitions: (&[u8], CatalogRowDecoder<TableSchema>),
        acls: (&[u8], CatalogRowDecoder<CatalogTableAcl>),
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::create(read, definitions, acls, None, limits, control)
    }

    /// Select independent ACL keys before reading their values when the provider's physical prefix also contains unrelated metadata.
    pub fn new_filtered(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        definitions: (&[u8], CatalogRowDecoder<TableSchema>),
        acls: (&[u8], CatalogRowDecoder<CatalogTableAcl>),
        select_acl: CatalogKeyFilter,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Self::create(read, definitions, acls, Some(select_acl), limits, control)
    }

    fn create(
        read: Arc<dyn KeyValueRead + Send + Sync>,
        definitions: (&[u8], CatalogRowDecoder<TableSchema>),
        acls: (&[u8], CatalogRowDecoder<CatalogTableAcl>),
        select_acl: Option<CatalogKeyFilter>,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<Input>())?;
        let definitions =
            CatalogInput::new(read.clone(), definitions.0, definitions.1, limits, control)?;
        let acls = if let Some(select) = select_acl {
            CatalogInput::new_filtered(read, acls.0, select, acls.1, limits, control)?
        } else {
            CatalogInput::new(read, acls.0, acls.1, limits, control)?
        };
        Ok(Self {
            control: control.clone(),
            state: State::Reading(Budgeted::new(
                Box::new(Input {
                    definitions: Some(definitions),
                    acls,
                }),
                memory,
            )),
        })
    }

    /// Consume at most one merged physical slot. Failure or unwinding retires both original streams; completion closes the common view and is idempotent.
    pub fn advance(&mut self) -> StorageBackendResult<CatalogTableInputStep> {
        match self.state {
            State::Complete => return Ok(CatalogTableInputStep::Complete),
            State::Failed => {
                return Err(StorageBackendError::Other(
                    "table catalog input is finished".into(),
                ))
            }
            State::Reading(_) => (),
        }
        let State::Reading(mut input) = std::mem::replace(&mut self.state, State::Failed) else {
            unreachable!()
        };
        let step = input.as_mut().advance()?;
        self.state = if matches!(step, CatalogTableInputStep::Complete) {
            drop(input);
            State::Complete
        } else {
            State::Reading(input)
        };
        Ok(step)
    }
}

impl Input {
    fn advance(&mut self) -> StorageBackendResult<CatalogTableInputStep> {
        if let Some(definitions) = self.definitions.as_mut() {
            return Ok(match definitions.advance()? {
                CatalogInputStep::Pending => CatalogTableInputStep::Pending,
                CatalogInputStep::Row(row) => {
                    let mut memory = row.budget().reserve(size_of::<TableSchema>())?;
                    let (row, payloads) = row.into_parts();
                    memory.absorb(payloads);
                    CatalogTableInputStep::Definition(Budgeted::new(Box::new(row), memory))
                }
                CatalogInputStep::Complete => {
                    self.definitions = None;
                    CatalogTableInputStep::DefinitionsComplete
                }
            });
        }
        Ok(match self.acls.advance()? {
            CatalogInputStep::Pending => CatalogTableInputStep::Pending,
            CatalogInputStep::Row(row) => CatalogTableInputStep::AclReplacement(row),
            CatalogInputStep::Complete => CatalogTableInputStep::Complete,
        })
    }
}

#[cfg(test)]
mod tests;
