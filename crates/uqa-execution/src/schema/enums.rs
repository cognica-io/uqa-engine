//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum type creation and label alteration: namespace reservation, generated array names, OID reservation and registry publication.

use super::namespaces::{
    relations::RelationCreationContext, NamespaceCatalogChanges, SchemaStatementWriter,
};
use crate::catalog::{
    enum_type::{self, EnumRegistryPublication},
    identity::{allocate_catalog_object_id, CatalogIdentityReservationContext},
    notices::CatalogNotices,
};
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterEnum, AlterEnumAction, CreateEnum},
    catalog::enum_type::{initial_enum_labels, AddedEnumLabel, StoredEnum},
    schema::{
        constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
        domains::removal::{resolve_alter_enum, TypeObjectBinding},
    },
    SQLError,
};

/// Labels that the current transaction added to types it did not create. `PostgreSQL` rejects their use until commit; labels of a type created by the same transaction are usable immediately. The owner discards this state when the outermost transaction ends.
#[derive(Debug, Default, Clone)]
pub struct UncommittedEnumLabels {
    created_types: std::collections::BTreeSet<u32>,
    added_labels: std::collections::BTreeSet<u32>,
}

impl UncommittedEnumLabels {
    pub fn type_created(&mut self, type_oid: u32) {
        self.created_types.insert(type_oid);
    }

    pub fn label_added(&mut self, type_oid: u32, label_oid: u32) {
        if !self.created_types.contains(&type_oid) {
            self.added_labels.insert(label_oid);
        }
    }

    pub fn is_uncommitted(&self, label_oid: u32) -> bool {
        self.added_labels.contains(&label_oid)
    }

    /// Forget the labels when the outermost transaction ends.
    pub fn clear(&mut self) {
        self.created_types.clear();
        self.added_labels.clear();
    }
}

/// Transaction-scoped usability of enum labels. A label added by `ALTER TYPE ... ADD VALUE` to a type that the current transaction did not create cannot be used until the transaction commits.
pub trait EnumLabelVisibility {
    fn enum_type_created(&self, type_oid: u32);
    fn enum_label_added(&self, type_oid: u32, label_oid: u32);
}

pub struct EnumTypeContext<'a> {
    pub creation: RelationCreationContext<'a>,
    pub identities: CatalogIdentityReservationContext<'a>,
    pub writer: &'a dyn SchemaStatementWriter,
    pub binding: TypeObjectBinding<'a>,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
    pub publication: &'a dyn EnumRegistryPublication,
    /// Domain and composite arrays share the type namespace with enum arrays.
    pub domains: &'a dyn crate::catalog::domain::DomainRegistryPublication,
    pub composites: &'a dyn crate::catalog::composite_type::CompositeRegistryPublication,
    pub changes: &'a dyn NamespaceCatalogChanges,
    pub visibility: &'a dyn EnumLabelVisibility,
    pub notices: &'a dyn CatalogNotices,
}

fn identity_error(
    error: &uqa_sql::schema::constraint_metadata::ConstraintMetadataError,
) -> SQLError {
    uqa_sql::catalog::errors::storage_error("enum catalog identity", error)
}

fn allocate_oid(
    allocator: &mut dyn CatalogObjectAllocator,
    class: CatalogOidClass,
    object_id: &[u8; 16],
) -> Result<u32, SQLError> {
    let oid = allocator
        .allocate_catalog_oid(class, object_id)
        .map_err(|error| identity_error(&error))?;
    u32::try_from(oid)
        .map_err(|_| SQLError::Internal(format!("invalid {} OID {oid}", class.label())))
}

fn label_oid(oid: i64) -> Result<u32, SQLError> {
    u32::try_from(oid).map_err(|_| SQLError::Internal(format!("invalid enum label OID {oid}")))
}

/// `CREATE TYPE ... AS ENUM`. A name held by a generated array type first moves that array out of the way, as `PostgreSQL`'s `moveArrayTypeName` does; any other existing type is a 42710 conflict. Label validation follows the name checks.
pub fn create_enum(context: &EnumTypeContext<'_>, definition: CreateEnum) -> Result<(), SQLError> {
    let owner = context.creation.bind_owner()?;
    context.writer.prepare_writer()?;
    let name = context.creation.persistent_name(&definition.name)?;
    let requested = RelationIdentity::from_legacy_name(&name).map_err(SQLError::Internal)?;
    super::types::arrays::displace_array_type(
        &context.creation,
        super::types::arrays::UserTypeRegistries {
            enums: context.publication,
            domains: context.domains,
            composites: context.composites,
        },
        &requested,
    )?;
    let identity = context.creation.reserve_type_name(&name)?;
    let array_name = super::types::arrays::reserve_array_name(
        &context.creation,
        &identity.schema,
        &identity.name,
    )?;
    let object_id = (context.allocate_identity)()?;
    // `DefineEnum` assigns the array type's OID before the enum's, and `EnumValuesCreate` then gives the labels even OIDs in ascending order.
    let mut allocator = context.identities.allocator(allocate_catalog_object_id);
    let array_oid = allocate_oid(&mut allocator, CatalogOidClass::Type, &object_id)?;
    let oid = allocate_oid(&mut allocator, CatalogOidClass::Type, &object_id)?;
    let mut label_oids = definition
        .labels
        .iter()
        .map(|_| {
            allocator
                .allocate_catalog_oid_matching(CatalogOidClass::EnumLabel, |oid| oid % 2 == 0)
                .and_then(label_oid)
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    label_oids.sort_unstable();
    let labels = initial_enum_labels(oid, &definition.labels, &label_oids)?;
    context.creation.retain_owner(&owner)?;
    let before = context.publication.enum_registry().clone();
    let mut registry = before.clone();
    registry.insert(
        identity.qualified_name(),
        StoredEnum {
            object_id,
            oid,
            array_oid,
            array_name,
            identity,
            owner: owner.identity(),
            labels,
            usage_acl: None,
        },
    );
    enum_type::publish(context.publication, &before, registry)?;
    context.visibility.enum_type_created(oid);
    context.changes.catalog_registry_changed();
    Ok(())
}

/// `ALTER TYPE ... ADD VALUE | RENAME VALUE`. Label changes on one type are serialized until the owning transaction ends; the definition is read again after the lock is held.
pub fn alter_enum(context: &EnumTypeContext<'_>, statement: AlterEnum) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    let resolved = resolve_alter_enum(&context.binding, &statement.name)?;
    let guard = context.identities.locks.acquire_shared_catalog(
        SharedCatalogLock::Object {
            class_id: CatalogOidClass::Type.class_id(),
            oid: resolved.oid,
        },
        RelationLockMode::AccessExclusive,
    )?;
    context.identities.locks.refresh_shared_catalog()?;
    guard.retain();
    let key = resolved.identity.qualified_name();
    let before = context.publication.enum_registry().clone();
    let mut registry = before.clone();
    let definition = registry
        .get_mut(&key)
        .filter(|definition| definition.oid == resolved.oid)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("type \"{}\" does not exist", resolved.identity.name),
        })?;
    match statement.action {
        AlterEnumAction::AddValue {
            label,
            if_not_exists,
            neighbor,
        } => {
            let mut allocator = context.identities.allocator(allocate_catalog_object_id);
            let allocate = |accept: &dyn Fn(u32) -> bool| {
                allocator
                    .allocate_catalog_oid_matching(CatalogOidClass::EnumLabel, |oid| {
                        u32::try_from(oid).is_ok_and(accept)
                    })
                    .and_then(label_oid)
            };
            match definition.add_label(&label, neighbor.as_ref(), if_not_exists, allocate)? {
                AddedEnumLabel::Added { oid, .. } => {
                    enum_type::publish(context.publication, &before, registry)?;
                    context.visibility.enum_label_added(resolved.oid, oid);
                }
                AddedEnumLabel::Skipped(message) => {
                    context
                        .notices
                        .notice(uqa_sql::SQLNotice::notice(message).with_sqlstate("42710"));
                    return Ok(());
                }
            }
        }
        AlterEnumAction::RenameValue { old, new } => {
            definition.rename_label(&old, &new)?;
            enum_type::publish(context.publication, &before, registry)?;
        }
    }
    context.changes.catalog_registry_changed();
    Ok(())
}
