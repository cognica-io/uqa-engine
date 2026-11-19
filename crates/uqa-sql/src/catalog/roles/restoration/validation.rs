//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered role checks share their original predicates and admitted scratch across resumable and ordinary restoration.

use super::super::{RoleDefinition, RoleDefinitions, RoleIdentity};
use crate::{catalog::named_definitions::NamedDefinitionIter, SQLError};
use uqa_core::{
    memory::{ProductionControl, ProductionMap},
    ValueRetentionError,
};

mod memberships;
pub(super) use memberships::MembershipChecks;
pub use memberships::{begin_membership_validation, RoleMembershipValidation};

#[derive(Debug, thiserror::Error)]
pub enum RoleRestorationError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Input(#[from] ValueRetentionError),
}

impl From<RoleRestorationError> for SQLError {
    fn from(error: RoleRestorationError) -> Self {
        match error {
            RoleRestorationError::Invalid(message) => Self::Internal(message),
            RoleRestorationError::Input(error) => error.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleValidationMode {
    /// Key/name agreement and OID validity follow bootstrap restoration.
    Definitions,
    /// Incarnations follow persisted OID claims and any initial-open migration.
    Identities,
    /// Tuple revisions follow incarnation validation.
    Revisions,
}

/// Every advance observes at most one role. The borrowed catalog stays immutable, and the original control owns every scratch node. Failure and unwinding retire the active traversal; successful completion releases scratch without revisiting entries.
pub struct RoleRegistryValidation<'a> {
    active: Option<Work<'a>>,
    complete: bool,
}

struct Work<'a> {
    entries: NamedDefinitionIter<'a, RoleDefinition>,
    control: ProductionControl<'a>,
    checks: Checks<'a>,
}

enum Checks<'a> {
    Definitions(ProductionMap<'a, i64, ()>),
    Identities {
        identities: ProductionMap<'a, [u8; 16], ()>,
        invalid_bootstrap: bool,
    },
    Revisions,
}

pub fn begin_validation<'a>(
    roles: &'a RoleDefinitions,
    mode: RoleValidationMode,
    control: ProductionControl<'a>,
) -> RoleRegistryValidation<'a> {
    RoleRegistryValidation {
        active: Some(Work {
            entries: roles.iter(),
            control,
            checks: Checks::new(mode, control),
        }),
        complete: false,
    }
}

impl RoleRegistryValidation<'_> {
    pub fn advance(&mut self) -> Result<bool, RoleRestorationError> {
        if self.complete {
            return Ok(true);
        }
        let mut work = self
            .active
            .take()
            .ok_or_else(|| invalid("role validation is finished"))?;
        work.control.check()?;
        if let Some((name, role)) = work.entries.next() {
            work.checks.validate(name, role)?;
            self.active = Some(work);
            Ok(false)
        } else {
            work.checks.finish()?;
            self.complete = true;
            Ok(true)
        }
    }
}

impl<'a> Checks<'a> {
    fn new(mode: RoleValidationMode, control: ProductionControl<'a>) -> Self {
        match mode {
            RoleValidationMode::Definitions => Checks::Definitions(ProductionMap::new(control)),
            RoleValidationMode::Identities => Checks::Identities {
                identities: ProductionMap::new(control),
                invalid_bootstrap: false,
            },
            RoleValidationMode::Revisions => Checks::Revisions,
        }
    }

    fn validate(&mut self, name: &str, role: &RoleDefinition) -> Result<(), RoleRestorationError> {
        match self {
            Self::Definitions(oids) => {
                if role.name != name {
                    return Err(invalid(format!(
                        "persisted role key `{name}` does not match role name `{}`",
                        role.name
                    )));
                }
                if role.oid <= 0 || role.oid > i64::from(u32::MAX) {
                    return Err(invalid(format!(
                        "persisted role `{name}` has an invalid OID {}",
                        role.oid
                    )));
                }
                if oids.insert_copy(role.oid, ())?.is_some() {
                    return Err(invalid(format!(
                        "persisted role OID {} is duplicated",
                        role.oid
                    )));
                }
            }
            Self::Identities {
                identities,
                invalid_bootstrap,
            } => {
                if role.object_id == [0; 16] {
                    return Err(invalid(format!(
                        "persisted role `{name}` has no object identity"
                    )));
                }
                if role.oid == 10 && role.object_id != RoleIdentity::BOOTSTRAP.object_id {
                    return Err(invalid(
                        "persisted bootstrap role has an invalid object identity",
                    ));
                }
                if identities.insert_copy(role.object_id, ())?.is_some() {
                    return Err(invalid("persisted role object identity is duplicated"));
                }
                *invalid_bootstrap |=
                    role.oid != 10 && role.object_id == RoleIdentity::BOOTSTRAP.object_id;
            }
            Self::Revisions => {
                if role.revision == 0 {
                    return Err(invalid(format!(
                        "persisted role `{name}` has no tuple revision"
                    )));
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<(), RoleRestorationError> {
        if matches!(
            self,
            Self::Identities {
                invalid_bootstrap: true,
                ..
            }
        ) {
            return Err(invalid(
                "persisted bootstrap role has an invalid object identity",
            ));
        }
        Ok(())
    }
}

pub(crate) fn finish(roles: &RoleDefinitions, mode: RoleValidationMode) -> Result<(), String> {
    let mut input = begin_validation(roles, mode, ProductionControl::uncontrolled());
    while !input.advance().map_err(|error| error.to_string())? {}
    Ok(())
}

fn invalid(message: impl Into<String>) -> RoleRestorationError {
    RoleRestorationError::Invalid(message.into())
}

#[cfg(test)]
mod tests;
