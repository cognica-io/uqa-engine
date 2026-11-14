//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted fields move into private native role roots without copying their payloads.

use super::{validation::MembershipChecks, RoleDefinitions, RoleMemberships};
use crate::{
    catalog::roles::{RoleDefinition, RoleMembership},
    plan::copy::SyntaxProduction,
    SQLError,
};
use uqa_core::memory::{Budgeted, BudgetedVec, OwnedProductionControl, ProductionControl};

pub struct RoleDefinitionInput {
    values: RoleDefinitions,
    bootstrap_oids: usize,
    bootstrap_names: usize,
    control: OwnedProductionControl,
}

impl RoleDefinitionInput {
    pub fn new(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        control.check()?;
        let memory = control
            .budget()
            .ok_or_else(|| SQLError::Internal("role input requires an admitted control".into()))?;
        Ok(Self {
            values: RoleDefinitions::with_budget(memory),
            bootstrap_oids: 0,
            bootstrap_names: 0,
            control: control.capture(),
        })
    }

    pub fn has_bootstrap_oid(&self) -> bool {
        self.bootstrap_oids != 0
    }

    /// Replacement retains the same final-key semantics as the native registry. Both bootstrap counters follow the successfully published root.
    pub fn insert(
        &mut self,
        name: Budgeted<String>,
        role: Budgeted<RoleDefinition>,
    ) -> Result<(), SQLError> {
        self.control.control().check()?;
        let (old_oid, old_name) = self.values.get(name.as_str()).map_or((0, 0), |previous| {
            (
                usize::from(previous.oid == 10),
                usize::from(previous.oid == 10 && previous.name == *name),
            )
        });
        let next_oid = usize::from(role.oid == 10);
        let next_name = usize::from(role.oid == 10 && role.name == *name);
        self.values.insert_image_entry(name, role)?;
        self.bootstrap_oids = self.bootstrap_oids - old_oid + next_oid;
        self.bootstrap_names = self.bootstrap_names - old_name + next_name;
        Ok(())
    }

    /// Bootstrap preparation precedes key/OID validation, exactly as in ordinary restoration. The returned catalog still requires the definition, OID-claim, incarnation and revision checks.
    pub fn finish(mut self) -> Result<RoleDefinitions, SQLError> {
        let control = self.control.control();
        control.check()?;
        if super::needs_bootstrap(&self.values, self.bootstrap_names != 0)
            .map_err(SQLError::Internal)?
        {
            let name = control
                .copy_text("uqa")?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("admitted role input"));
            let role = RoleDefinition::bootstrap_with_control(&control)?
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("admitted role input"));
            self.values.insert_image_entry(name, role)?;
        }
        Ok(self.values)
    }
}

impl RoleDefinition {
    pub(in crate::catalog::roles) fn bootstrap_with_control(
        control: &ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Self>, SQLError> {
        use crate::ast::RoleAttribute;
        let output = SyntaxProduction::new(*control);
        let value = Self {
            oid: 10,
            object_id: super::super::RoleIdentity::BOOTSTRAP.object_id,
            revision: 1,
            name: output.text("uqa")?,
            attributes: output.set_copy([
                RoleAttribute::Superuser,
                RoleAttribute::Inherit,
                RoleAttribute::CreateRole,
                RoleAttribute::CreateDb,
                RoleAttribute::Login,
                RoleAttribute::BypassRls,
            ])?,
            connection_limit: -1,
        };
        output.finish(value)
    }
}

struct MembershipRecords {
    entries: std::vec::IntoIter<Budgeted<RoleMembership>>,
    _memory: uqa_core::memory::MemoryReservation,
}

/// Stored fields and persisted record keys precede this boundary. Seed one original role or validate and publish one membership per checkpoint; partial roots remain private until every record succeeds.
pub async fn restore_membership_records(
    roles: &RoleDefinitions,
    input: BudgetedVec<Budgeted<RoleMembership>>,
    control: &ProductionControl<'_>,
    reader: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> Result<RoleMemberships, SQLError> {
    control.check()?;
    let memory = control.budget().ok_or_else(|| {
        SQLError::Internal("role membership input requires an admitted control".into())
    })?;
    assert!(memory.shares_allowance(input.budget()));
    let mut checks = MembershipChecks::new(*control);
    for role in roles.values() {
        reader.checkpoint().await?;
        checks.role(role).map_err(SQLError::from)?;
    }
    let (entries, memory) = input.into_parts();
    let mut input = MembershipRecords {
        entries: entries.into_iter(),
        _memory: memory,
    };
    let mut values = RoleMemberships::with_budget(control.budget().expect("admitted role input"));
    for value in input.entries.by_ref() {
        reader.checkpoint().await?;
        control.check()?;
        checks.membership(&value).map_err(SQLError::from)?;
        let key = Budgeted::new(
            value.key(),
            control
                .budget()
                .expect("admitted role input")
                .empty_reservation(),
        );
        values.insert_image_entry(key, value)?;
    }
    control.check()?;
    Ok(values)
}

#[cfg(test)]
mod tests;
