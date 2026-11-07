//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native domain validation retains admitted identity scratch across bounded catalog visits.

use super::{domain_object_oid, StoredDomain};
use crate::catalog::{
    named_definitions::NamedDefinitionView,
    roles::{RoleDefinition, RoleDefinitions, RoleIdentity},
    type_definitions::TypeDefinitionView,
};
use crate::schema::{
    constraint_metadata::{ConstraintMetadataError, ConstraintMetadataResult},
    domains::constraints::{constraint, ConstraintValidation},
};
use uqa_core::{
    memory::{ProductionControl, ProductionMap},
    RelationIdentity, RelationName,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainValidationMode {
    /// Validate supplied legacy metadata before any missing constraint identities are allocated.
    Definitions,
    /// Also require complete durable names and identities after all supplied metadata is validated.
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainValidationStep {
    Pending,
    Complete,
}

/// Retain the immutable catalog and roles while validation runs. Construction does not visit entries or allocate scratch. Each advance checks at most one domain header, one role identity or one constraint; all identity-map allocations use the original control. Role indexing stops at the selected owner and resumes only for an unseen owner. Failure, cancellation or unwinding retires scratch and makes this continuation terminal. Completion retires scratch and remains observable without traversing the catalog again.
pub struct DomainRegistryValidation<'a, I, R> {
    active: Option<Work<'a, I, R>>,
    complete: bool,
}

pub fn begin_validation<'a>(
    registry: &'a impl TypeDefinitionView<StoredDomain>,
    roles: &'a impl NamedDefinitionView<RoleDefinition>,
    mode: DomainValidationMode,
    control: ProductionControl<'a>,
) -> DomainRegistryValidation<
    'a,
    impl Iterator<Item = (&'a String, &'a StoredDomain)>,
    impl Iterator<Item = (&'a String, &'a RoleDefinition)>,
> {
    DomainRegistryValidation {
        active: Some(Work {
            entries: registry.iter(),
            required: (mode == DomainValidationMode::Complete).then(|| registry.iter()),
            roles: roles.iter(),
            role_index: ProductionMap::new(control),
            control,
            identities: ProductionMap::new(control),
            oids: ProductionMap::new(control),
            constraint_objects: ProductionMap::new(control),
            constraint_oids: ProductionMap::new(control),
            current: None,
            required_phase: false,
        }),
        complete: false,
    }
}

impl<'a, I, R> DomainRegistryValidation<'a, I, R>
where
    I: Iterator<Item = (&'a String, &'a StoredDomain)>,
    R: Iterator<Item = (&'a String, &'a RoleDefinition)>,
{
    pub fn advance(&mut self) -> ConstraintMetadataResult<DomainValidationStep> {
        if self.complete {
            return Ok(DomainValidationStep::Complete);
        }
        // The owner retires the traversal on every failure, including an unwound iterator.
        let mut work = self
            .active
            .take()
            .ok_or_else(|| invalid("domain validation is finished"))?;
        work.control.check().map_err(execution)?;
        if work.advance()? {
            self.complete = true;
            Ok(DomainValidationStep::Complete)
        } else {
            self.active = Some(work);
            Ok(DomainValidationStep::Pending)
        }
    }
}

struct Work<'a, I, R> {
    entries: I,
    required: Option<I>,
    roles: R,
    role_index: ProductionMap<'a, RoleIdentity, ()>,
    control: ProductionControl<'a>,
    identities: ProductionMap<'a, [u8; 16], ()>,
    oids: ProductionMap<'a, u32, ()>,
    constraint_objects: ProductionMap<'a, [u8; 16], ()>,
    constraint_oids: ProductionMap<'a, i64, ()>,
    current: Option<Current<'a>>,
    required_phase: bool,
}

enum Current<'a> {
    Owner {
        name: &'a str,
        domain: &'a StoredDomain,
    },
    Local {
        name: &'a str,
        domain: &'a StoredDomain,
        validation: ConstraintValidation<'a>,
    },
    Global {
        name: &'a str,
        domain: &'a StoredDomain,
        index: usize,
    },
    Required(ConstraintValidation<'a>),
}

impl<'a, I, R> Work<'a, I, R>
where
    I: Iterator<Item = (&'a String, &'a StoredDomain)>,
    R: Iterator<Item = (&'a String, &'a RoleDefinition)>,
{
    fn advance(&mut self) -> ConstraintMetadataResult<bool> {
        match self.current.take() {
            Some(Current::Owner { name, domain }) => {
                self.current = Some(self.owner_step(name, domain)?);
            }
            Some(Current::Local {
                name,
                domain,
                mut validation,
            }) => {
                self.current = Some(if validation.advance()? {
                    Current::Global {
                        name,
                        domain,
                        index: 0,
                    }
                } else {
                    Current::Local {
                        name,
                        domain,
                        validation,
                    }
                });
            }
            Some(Current::Global {
                name,
                domain,
                index,
            }) => {
                if let Some((_, identity)) = constraint(&domain.definition, index) {
                    if let Some(identity) = identity {
                        if self
                            .constraint_objects
                            .insert_copy(identity.object_id, ())
                            .map_err(execution)?
                            .is_some()
                            || self
                                .constraint_oids
                                .insert_copy(identity.oid, ())
                                .map_err(execution)?
                                .is_some()
                        {
                            return Err(invalid(format!(
                                "duplicate domain constraint catalog identity for `{name}`"
                            )));
                        }
                    }
                    self.current = Some(Current::Global {
                        name,
                        domain,
                        index: index + 1,
                    });
                }
            }
            Some(Current::Required(mut validation)) => {
                if !validation.advance()? {
                    self.current = Some(Current::Required(validation));
                }
            }
            None if self.required_phase => {
                let Some((_, domain)) = self.required.as_mut().and_then(Iterator::next) else {
                    return Ok(true);
                };
                self.current = Some(Current::Required(ConstraintValidation::new(
                    &domain.definition,
                    false,
                    self.control,
                )));
            }
            None => {
                if let Some((name, domain)) = self.entries.next() {
                    self.header(name, domain)?;
                    self.current = Some(Current::Owner { name, domain });
                } else if self.required.is_some() {
                    self.required_phase = true;
                } else {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn owner_step(
        &mut self,
        name: &'a str,
        domain: &'a StoredDomain,
    ) -> ConstraintMetadataResult<Current<'a>> {
        if !domain.owner.is_valid() {
            return Err(missing_owner(name, domain.owner));
        }
        if self.role_index.get(&domain.owner).is_none() {
            let Some((_, role)) = self.roles.next() else {
                return Err(missing_owner(name, domain.owner));
            };
            self.role_index
                .insert_copy(role.identity(), ())
                .map_err(execution)?;
            return Ok(Current::Owner { name, domain });
        }
        Ok(Current::Local {
            name,
            domain,
            validation: ConstraintValidation::new(&domain.definition, true, self.control),
        })
    }

    fn header(&mut self, name: &str, domain: &StoredDomain) -> ConstraintMetadataResult<()> {
        // Recorded array OIDs share the database allocator; legacy type OIDs derive from the original incarnation.
        let oids_valid = match domain.array_oid {
            Some(array_oid) => {
                domain.oid >= crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
                    && array_oid >= crate::catalog::oids::FIRST_NORMAL_OBJECT_ID
                    && self
                        .oids
                        .insert_copy(array_oid, ())
                        .map_err(execution)?
                        .is_none()
            }
            None => domain.oid == domain_object_oid(&domain.object_id),
        };
        if domain.object_id == [0; 16]
            || self
                .identities
                .insert_copy(domain.object_id, ())
                .map_err(execution)?
                .is_some()
            || !oids_valid
            || self
                .oids
                .insert_copy(domain.oid, ())
                .map_err(execution)?
                .is_some()
        {
            return Err(invalid(format!(
                "invalid or duplicate domain identity for `{name}`"
            )));
        }
        let canonical = domain
            .identity
            .qualified_name_with_control(&self.control)
            .map_err(execution)?;
        let declaration_matches = RelationIdentity::borrowed_reference(&domain.definition.name)
            .is_some_and(|reference| {
                reference
                    .schema
                    .unwrap_or_else(|| RelationName::decoded("public"))
                    == RelationName::decoded(&domain.identity.schema)
                    && reference.name == RelationName::decoded(&domain.identity.name)
            });
        if domain.identity.schema.is_empty()
            || domain.identity.name.is_empty()
            || canonical.as_str() != name
            || !declaration_matches
        {
            return Err(invalid(format!("inconsistent domain name for `{name}`")));
        }
        self.control.check().map_err(execution)?;
        Ok(())
    }
}

fn missing_owner(name: &str, identity: RoleIdentity) -> ConstraintMetadataError {
    invalid(format!(
        "domain `{name}` references missing role incarnation {}",
        identity.oid
    ))
}

fn invalid(message: impl Into<String>) -> ConstraintMetadataError {
    ConstraintMetadataError::Invalid(message.into())
}

fn execution(error: uqa_core::ValueRetentionError) -> ConstraintMetadataError {
    ConstraintMetadataError::Execution(Box::new(error.into()))
}

pub(super) fn finish(
    registry: &impl TypeDefinitionView<StoredDomain>,
    roles: &RoleDefinitions,
    mode: DomainValidationMode,
) -> Result<(), String> {
    let mut validation = begin_validation(registry, roles, mode, ProductionControl::uncontrolled());
    while validation.advance().map_err(|error| error.to_string())? != DomainValidationStep::Complete
    {
    }
    Ok(())
}

#[cfg(test)]
mod tests;
