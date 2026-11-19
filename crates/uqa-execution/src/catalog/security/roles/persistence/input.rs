//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Current persisted role catalogs retain their original row view through complete private restoration.

use super::{
    memberships, records, RoleCatalogValues, ROLES_METADATA_KEY, ROLE_MEMBERSHIPS_METADATA_KEY,
};
use std::{fmt::Write, sync::Arc};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecodeWindow, ImageDecoder},
    memory::{
        Budgeted, BudgetedMap, BudgetedVec, MemoryBudget, Produced, ProductionControl,
        ProductionString,
    },
};
use uqa_sql::{
    catalog::roles::{
        restoration::{
            input::{restore_membership_records, RoleDefinitionInput},
            validation::{begin_validation, RoleRestorationError, RoleValidationMode},
        },
        RoleDefinition, RoleDefinitions, RoleMembership, RoleMembershipKey, RoleMemberships,
    },
    SQLError,
};
use uqa_storage::{
    catalog::text_input::{CatalogTextInput, CatalogTextLimits, CatalogTextRow, CatalogTextStep},
    read_control::StorageReadControl,
    CatalogFacade, StorageBackendError, StorageBackendResult,
};

pub struct RoleCatalogInput {
    input: ImageDecoder<RoleCatalogValues, StorageBackendError>,
}

impl RoleCatalogInput {
    /// Restored images contain current records. Initial-open migrations stay with ordinary catalog restoration. The caller retains one read transaction across this complete operation; no metadata is written and no partial catalog is published.
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        limits: CatalogTextLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut reader = reader.input();
                let control = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    reader.cancellation,
                );
                let roles = read_roles(&*catalog, limits, &retained, &control, &mut reader).await?;
                let members =
                    read_memberships(&*catalog, &roles, limits, &retained, &control, &mut reader)
                        .await?;
                control.check()?;
                Ok(RoleCatalogValues {
                    roles: roles.into_mutation_budget(&mutations),
                    memberships: members.into_mutation_budget(&mutations),
                })
            },
        )?;
        Ok(Self { input })
    }

    /// Bound provider slots, individual role checks and native publication by the existing continuation window. Errors, cancellation, zero windows and unwinding consume every unpublished owner.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<RoleCatalogValues>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn next_row(
    input: &mut CatalogTextInput,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Option<Budgeted<CatalogTextRow>>> {
    loop {
        reader.checkpoint().await?;
        match input.advance()? {
            CatalogTextStep::Row(row) => return Ok(Some(row)),
            CatalogTextStep::Complete => return Ok(None),
            CatalogTextStep::Pending => (),
        }
    }
}

async fn validate(
    roles: &RoleDefinitions,
    mode: RoleValidationMode,
    control: &ProductionControl<'_>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let mut validation = begin_validation(roles, mode, *control);
    loop {
        reader.checkpoint().await?;
        if validation.advance().map_err(validation_error)? {
            return Ok(());
        }
    }
}

async fn read_roles(
    catalog: &dyn CatalogFacade,
    limits: CatalogTextLimits,
    retained: &StorageReadControl,
    control: &ProductionControl<'_>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<RoleDefinitions> {
    reader.checkpoint().await?;
    let marker = catalog.get_metadata_with_control(ROLES_METADATA_KEY, limits, retained)?;
    records::require_current_marker(marker.as_deref().map(String::as_str))?;
    drop(marker);
    let mut input = catalog.begin_metadata_input(records::ROLE_PREFIX, limits, retained)?;
    let mut roles = RoleDefinitionInput::new(control).map_err(sql_error)?;
    while let Some(row) = next_row(&mut input, reader).await? {
        let name = control
            .copy_text(&row.name[records::ROLE_PREFIX.len()..])?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("admitted role input"));
        let role = RoleDefinition::from_stored_json_with_control(&row.value, control)
            .map_err(sql_error)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("admitted role input"));
        roles.insert(name, role).map_err(sql_error)?;
    }
    if !roles.has_bootstrap_oid() {
        return Err(invalid("role records are missing the bootstrap role"));
    }
    let roles = roles.finish().map_err(sql_error)?;
    validate(&roles, RoleValidationMode::Definitions, control, reader).await?;
    let mut claims = catalog.begin_metadata_input(records::OID_PREFIX, limits, retained)?;
    let mut count = 0;
    while let Some(row) = next_row(&mut claims, reader).await? {
        let role = roles
            .get(row.value.as_str())
            .ok_or_else(|| invalid("role OID records do not match role definitions"))?;
        if row.name != *oid_key(records::OID_PREFIX, role.oid, control)? {
            return Err(invalid("role OID records do not match role definitions"));
        }
        count += 1;
    }
    if count != roles.len() {
        return Err(invalid("role OID records do not match role definitions"));
    }
    validate(&roles, RoleValidationMode::Identities, control, reader).await?;
    validate(&roles, RoleValidationMode::Revisions, control, reader).await?;
    Ok(roles)
}

async fn read_memberships(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
    limits: CatalogTextLimits,
    retained: &StorageReadControl,
    control: &ProductionControl<'_>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<RoleMemberships> {
    reader.checkpoint().await?;
    let marker =
        catalog.get_metadata_with_control(ROLE_MEMBERSHIPS_METADATA_KEY, limits, retained)?;
    memberships::require_current_marker(marker.as_deref().map(String::as_str))?;
    drop(marker);
    let mut rows = catalog.begin_metadata_input(memberships::PREFIX, limits, retained)?;
    let mut values = BudgetedVec::new(retained.memory());
    while let Some(row) = next_row(&mut rows, reader).await? {
        let value = RoleMembership::from_stored_json_with_control(&row.value, control)
            .map_err(sql_error)?;
        if row.name != *member_key(&value.key(), control)? {
            return Err(invalid(
                "role membership record key does not match its role identities",
            ));
        }
        values.push(
            value
                .into_budgeted()
                .unwrap_or_else(|_| unreachable!("admitted role input")),
        )?;
    }
    let values = restore_membership_records(roles, values, control, reader)
        .await
        .map_err(sql_error)?;
    validate_membership_oids(catalog, &values, limits, retained, control, reader).await?;
    Ok(values)
}

async fn validate_membership_oids(
    catalog: &dyn CatalogFacade,
    values: &RoleMemberships,
    limits: CatalogTextLimits,
    retained: &StorageReadControl,
    control: &ProductionControl<'_>,
    reader: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let mut owners = BudgetedMap::new(retained.memory());
    for (key, membership) in values {
        reader.checkpoint().await?;
        owners.insert(membership.oid, *key)?;
    }
    let mut claims = catalog.begin_metadata_input(memberships::OID_PREFIX, limits, retained)?;
    let mut count = 0;
    while let Some(row) = next_row(&mut claims, reader).await? {
        let failure = || invalid("role membership OID records do not match membership identities");
        let oid = row.name[memberships::OID_PREFIX.len()..]
            .parse::<i64>()
            .map_err(|_| failure())?;
        let key = owners.get(&oid).ok_or_else(failure)?;
        if row.name != *oid_key(memberships::OID_PREFIX, oid, control)?
            || row.value != *member_key(key, control)?
        {
            return Err(failure());
        }
        count += 1;
    }
    if count != owners.len() {
        return Err(invalid(
            "role membership OID records do not match membership identities",
        ));
    }
    Ok(())
}

fn oid_key(
    prefix: &str,
    oid: i64,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<String>> {
    let mut text = ProductionString::new(*control);
    let _ = write!(text, "{prefix}{oid}");
    Ok(text.finish()?)
}

fn member_key(
    key: &RoleMembershipKey,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<String>> {
    let mut text = ProductionString::new(*control);
    let _ = memberships::write_key(&mut text, key);
    Ok(text.finish()?)
}

fn validation_error(error: RoleRestorationError) -> StorageBackendError {
    match error {
        RoleRestorationError::Invalid(message) => StorageBackendError::Other(message),
        RoleRestorationError::Input(error) => error.into(),
    }
}
fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        SQLError::Internal(message) => StorageBackendError::Other(message),
        error => StorageBackendError::backend("stored role catalog", error),
    }
}
fn invalid(message: &'static str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

#[cfg(test)]
mod tests;
