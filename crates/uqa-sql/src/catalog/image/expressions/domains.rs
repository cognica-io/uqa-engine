//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain images borrow one immutable catalog and retain every original declaration field.

use super::{DefinitionImageReader, Field, Fields, Frame, State};
use crate::{
    ast::ConstraintCatalogIdentity,
    catalog::{domain::StoredDomain, type_definitions::CapturedTypeDefinitions},
    SQLError,
};
use uqa_core::{
    memory::{BudgetedVec, MemoryBudget},
    CancellationToken,
};

impl CapturedTypeDefinitions<StoredDomain> {
    /// Emit native domain definitions from this captured root. Temporary definitions are already excluded by capture; names, evaluated constants, constraints and authority are never rebound or executed.
    pub fn image_reader<'a>(
        &'a self,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<DefinitionImageReader<'a>, SQLError> {
        cancellation.check()?;
        let mut fields = Fields::new();
        fields.scalar(*b"UQADOM01");
        fields.count(self.len());
        Ok(DefinitionImageReader {
            first: Some(Frame::NextDomain),
            statement_expressions: false,
            domains: Some(self.iter()),
            views: None,
            frames: BudgetedVec::new(memory),
            fields,
            field: Field::Bytes(&[]),
            offset: 0,
            child: None,
            memory,
            cancellation,
            state: State::Active,
        })
    }
}

impl crate::catalog::type_definitions::TypeDefinitions<StoredDomain> {
    /// Reconstruct an unpublished domain catalog with original metadata and admitted native scalar definitions. Cross-catalog identity, authority and dependency validation remains with the enclosing restore owner.
    pub fn read_image(
        reader: &mut dyn std::io::Read,
        memory: &MemoryBudget,
        cancellation: &CancellationToken,
    ) -> Result<Self, SQLError> {
        super::super::input::domains(reader, memory, cancellation)
    }
}

impl<'a> DefinitionImageReader<'a> {
    pub(super) fn domain_part(&mut self, frame: Frame<'a>) -> Result<(), SQLError> {
        match frame {
            Frame::NextDomain => {
                if let Some((name, value)) = self.domains.as_mut().and_then(Iterator::next) {
                    self.fields.text(name);
                    self.domain_header(value);
                    self.push(Frame::NextDomain)?;
                    self.push(Frame::DomainDefinition(&value.definition))?;
                    if let Some(acl) = &value.usage_acl {
                        self.push(Frame::ObjectAcls(acl))?;
                    }
                }
            }
            Frame::ObjectAcls(entries) => {
                if let Some((first, rest)) = entries.split_first() {
                    self.fields.object_acl(first);
                    self.push(Frame::ObjectAcls(rest))?;
                }
            }
            Frame::DomainDefinition(value) => {
                let crate::ast::CreateDomain {
                    name,
                    base,
                    collation,
                    collation_oid,
                    default,
                    not_null,
                    checks,
                } = value;
                self.fields.text(name);
                self.fields.optional_text(collation.as_deref());
                self.fields.optional_u32(*collation_oid);
                self.fields.flag(default.is_some());
                self.fields.flag(not_null.is_some());
                self.fields.count(checks.len());
                self.push(Frame::DomainChecks(checks))?;
                self.optional_node(default.as_ref())?;
                if let Some(value) = not_null {
                    self.push(Frame::DomainNotNull(value))?;
                }
                self.push(Frame::Type(base))?;
            }
            Frame::DomainNotNull(value) => {
                let crate::ast::DomainNotNull {
                    name,
                    catalog_identity,
                } = value;
                self.fields.optional_text(name.as_deref());
                self.constraint_identity(*catalog_identity);
            }
            Frame::DomainChecks(values) => {
                if let Some((first, rest)) = values.split_first() {
                    let crate::ast::DomainCheck {
                        name,
                        catalog_identity,
                        expression,
                        validated,
                    } = first;
                    self.fields.optional_text(name.as_deref());
                    self.constraint_identity(*catalog_identity);
                    self.fields.flag(*validated);
                    self.push(Frame::DomainChecks(rest))?;
                    self.push(Frame::Node(expression))?;
                }
            }
            _ => unreachable!("domain image continuation"),
        }
        Ok(())
    }

    fn domain_header(&mut self, value: &'a StoredDomain) {
        let StoredDomain {
            object_id,
            oid,
            array_oid,
            identity,
            owner,
            definition: _,
            array_name,
            usage_acl,
        } = value;
        self.fields.scalar(*object_id);
        self.fields.scalar(oid.to_le_bytes());
        self.fields.optional_u32(*array_oid);
        self.fields.text(&identity.schema);
        self.fields.text(&identity.name);
        self.fields.identity(*owner);
        self.fields.optional_text(array_name.as_deref());
        self.fields.flag(usage_acl.is_some());
        if let Some(acl) = usage_acl {
            self.fields.count(acl.len());
        }
    }

    pub(super) fn constraint_identity(&mut self, value: Option<ConstraintCatalogIdentity>) {
        self.fields.flag(value.is_some());
        if let Some(ConstraintCatalogIdentity { object_id, oid }) = value {
            self.fields.scalar(object_id);
            self.fields.scalar(oid.to_le_bytes());
        }
    }
}
