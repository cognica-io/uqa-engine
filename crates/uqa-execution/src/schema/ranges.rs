//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a declared range, its multirange, array types and constructor functions in the enclosing definition transaction.

use crate::catalog::{context::CatalogContext, identity::allocate_catalog_object_id, range_type};
use crate::routines::registration::RoutineRegistrationContext;
use crate::schema::{
    shell_types::ShellCompletionContext,
    types::{arrays, TypeLifecycleContext},
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{CreateRange, RangeOptionValue},
    catalog::{
        array_type_names::multirange_type_name,
        range_type::{StoredMultirange, StoredRange},
        type_metadata,
    },
    schema::constraint_metadata::{CatalogObjectAllocator, CatalogOidClass},
    ColumnType, SQLError,
};

mod support;

pub struct RangeCreationContext<'a> {
    pub types: TypeLifecycleContext<'a>,
    pub catalog: CatalogContext<'a>,
    pub shells: ShellCompletionContext<'a>,
    pub routines: RoutineRegistrationContext<'a>,
    pub allocate_identity: fn() -> Result<[u8; 16], SQLError>,
}

fn error(state: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: state.into(),
        message: message.into(),
    }
}

fn identity(name: &str) -> Result<RelationIdentity, SQLError> {
    RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)
}

#[derive(Default)]
struct Options<'a> {
    subtype: Option<ColumnType>,
    class: Option<&'a str>,
    collation: Option<&'a str>,
    canonical: Option<&'a str>,
    difference: Option<&'a str>,
    multirange: Option<RelationIdentity>,
}

struct ResolvedOptions {
    subtype: ColumnType,
    subtype_operator_class: u32,
    collation_oid: u32,
    canonical_oid: Option<u32>,
    subtype_difference_oid: Option<u32>,
    multirange: Option<RelationIdentity>,
}

impl<'a> Options<'a> {
    fn bind(
        context: &RangeCreationContext<'_>,
        declaration: &'a CreateRange,
    ) -> Result<Self, SQLError> {
        let mut options = Self::default();
        let types = context.routines.definition.compilation.analysis.types;
        for option in &declaration.options {
            let duplicate =
                || error("42601", "conflicting or redundant options").at_position(option.position);
            let name = || match &option.value {
                RangeOptionValue::Name(name) => Ok(name.as_str()),
                _ => Err(error("42601", format!("{} requires a name", option.name))),
            };
            let slot = match option.name.as_str() {
                "subtype" => {
                    if options.subtype.is_some() {
                        return Err(duplicate());
                    }
                    let RangeOptionValue::Type(ty) = &option.value else {
                        return Err(error("42601", "subtype requires a type name"));
                    };
                    let subtype = types.resolve_catalog_column_type_name(&ty.sql_name())?;
                    options.subtype = Some(subtype.without_type_modifiers());
                    continue;
                }
                "multirange_type_name" => {
                    if options.multirange.is_some() {
                        return Err(duplicate());
                    }
                    options.multirange =
                        Some(identity(&context.types.creation.object_name(name()?)?)?);
                    continue;
                }
                "collation" => &mut options.collation,
                "subtype_opclass" => &mut options.class,
                "canonical" => &mut options.canonical,
                "subtype_diff" => &mut options.difference,
                unknown => {
                    return Err(error(
                        "42601",
                        format!("type attribute \"{unknown}\" not recognized"),
                    ))
                }
            };
            if slot.is_some() {
                return Err(duplicate());
            }
            *slot = Some(name()?);
        }
        Ok(options)
    }

    fn resolve(
        self,
        context: &RangeCreationContext<'_>,
        shell_oid: Option<u32>,
    ) -> Result<ResolvedOptions, SQLError> {
        let Options {
            subtype,
            class,
            collation,
            canonical,
            difference,
            multirange,
        } = self;
        let subtype =
            subtype.ok_or_else(|| error("42601", "type attribute \"subtype\" is required"))?;
        let subtype_oid = type_metadata::pg_type_oid(&subtype);
        if matches!(
            subtype,
            ColumnType::Record | ColumnType::AnyArray | ColumnType::Void | ColumnType::Pseudo(_)
        ) || type_metadata::routine_pseudo_type_name(subtype_oid).is_some()
        {
            return Err(error(
                "42804",
                format!("range subtype cannot be {}", subtype.display_name()),
            ));
        }
        let subtype_operator_class = support::operator_class(context, &subtype, class)?;
        if collation.is_some() && type_metadata::pg_type_collation_oid(&subtype) == 0 {
            return Err(error(
                "42809",
                "range collation specified but subtype does not support collation",
            ));
        }
        let collation_oid = uqa_sql::catalog::collations::declarations::select(
            context.catalog.routines,
            &subtype,
            collation,
        )?;
        let canonical_oid = canonical.map(|name| {
            let oid = shell_oid.ok_or_else(|| SQLError::Diagnostic {
                sqlstate: "42P17".into(),
                message: "cannot specify a canonical function without a pre-created shell type".into(),
                detail: None, hint: Some("Create the type as a shell type, then create its canonicalization function, then do a full CREATE TYPE.".into()),
            })?;
            support::routine(context, name, oid, true)
        }).transpose()?;
        let subtype_difference_oid = difference
            .map(|name| {
                support::routine(
                    context,
                    name,
                    u32::try_from(subtype_oid).expect("type OID"),
                    false,
                )
            })
            .transpose()?;

        Ok(ResolvedOptions {
            subtype,
            subtype_operator_class,
            collation_oid,
            canonical_oid,
            subtype_difference_oid,
            multirange,
        })
    }
}

impl RangeCreationContext<'_> {
    /// Rows inserted by one declaration can conflict at the unique index before they become visible to name lookup.
    fn reserve_related_type(
        &self,
        name: RelationIdentity,
        pending: &[&RelationIdentity],
    ) -> Result<RelationIdentity, SQLError> {
        if pending.contains(&&name) {
            let namespace = crate::catalog::projection::schema_object_oid(
                &self.catalog.catalog_read_view(),
                &name.schema,
            );
            return Err(SQLError::Diagnostic {
                sqlstate: "23505".into(),
                message:
                    "duplicate key value violates unique constraint \"pg_type_typname_nsp_index\""
                        .into(),
                detail: Some(format!(
                    "Key (typname, typnamespace)=({}, {namespace}) already exists.",
                    name.name
                )),
                hint: None,
            });
        }
        self.types
            .creation
            .reserve_type_name(&name.qualified_name())
    }
}

fn reserve_multirange_name(
    context: &RangeCreationContext<'_>,
    range: &RelationIdentity,
    requested: Option<RelationIdentity>,
) -> Result<RelationIdentity, SQLError> {
    let types = &context.types;
    let creation = &types.creation;
    let name = if let Some(name) = requested {
        arrays::displace_array_type(creation, types.registries, &name)?;
        name
    } else {
        let name = RelationIdentity::new(&range.schema, multirange_type_name(&range.name));
        if &name == range || creation.type_name_in_use(&name) {
            return Err(SQLError::Diagnostic {
                sqlstate: "42710".into(), message: format!("type \"{}\" already exists", name.name),
                detail: Some(format!("Failed while creating a multirange type for type \"{}\".", range.name)),
                hint: Some("You can manually specify a multirange type name using the \"multirange_type_name\" attribute.".into()),
            });
        }
        name
    };
    context.reserve_related_type(name, &[range])
}

pub fn create_range(
    context: &RangeCreationContext<'_>,
    declaration: &CreateRange,
) -> Result<(), SQLError> {
    let types = &context.types;
    let creation = &types.creation;
    let owner = creation.bind_owner()?;
    types.writer.prepare_writer()?;
    let name = creation.object_name(&declaration.name)?;
    let requested = identity(&name)?;
    let shell = context.shells.retain(&name, owner.identity())?;
    arrays::displace_array_type(creation, types.registries, &requested)?;
    let range = if let Some(shell) = &shell {
        shell.identity.clone()
    } else {
        creation.reserve_type_name(&name)?
    };
    let ResolvedOptions {
        subtype,
        subtype_operator_class,
        collation_oid,
        canonical_oid,
        subtype_difference_oid,
        multirange,
    } = Options::bind(context, declaration)?
        .resolve(context, shell.as_ref().map(|shell| shell.oid))?;
    let multi = reserve_multirange_name(context, &range, multirange)?;
    let reserve_array = |element: &RelationIdentity, pending: &[&RelationIdentity]| {
        let name = arrays::available_array_name(creation, &range.schema, &element.name);
        context.reserve_related_type(RelationIdentity::new(&element.schema, name), pending)
    };
    let range_array = reserve_array(&range, &[&range, &multi])?;
    let multi_array = reserve_array(&multi, &[&range, &multi, &range_array])?;
    let object_id = shell.as_ref().map_or_else(
        || (context.allocate_identity)(),
        |shell| Ok(shell.object_id),
    )?;
    let multi_object_id = (context.allocate_identity)()?;
    let mut allocator = types.identities.allocator(allocate_catalog_object_id);
    let mut allocate = |class, id: &[u8; 16]| -> Result<u32, SQLError> {
        let oid = allocator
            .allocate_catalog_oid(class, id)
            .map_err(|failure| {
                uqa_sql::catalog::errors::storage_error("range catalog identity", &failure)
            })?;
        u32::try_from(oid)
            .map_err(|_| SQLError::Internal(format!("invalid range catalog OID {oid}")))
    };
    let array_oid = allocate(CatalogOidClass::Type, &object_id)?;
    let multi_oid = allocate(CatalogOidClass::Type, &multi_object_id)?;
    let multi_array_oid = allocate(CatalogOidClass::Type, &multi_object_id)?;
    let oid = shell.as_ref().map_or_else(
        || allocate(CatalogOidClass::Type, &object_id),
        |shell| Ok(shell.oid),
    )?;
    creation.retain_owner(&owner)?;
    let mut definition = StoredRange {
        object_id,
        oid,
        array_oid,
        array_name: range_array.name,
        owner: owner.identity(),
        usage_acl: creation.defaults.type_acl(owner.identity(), &range.schema),
        subtype,
        subtype_operator_class,
        collation_oid,
        canonical_oid,
        subtype_difference_oid,
        constructor_oids: None,
        cast_oid: None,
        identity: range,
        multirange: StoredMultirange {
            object_id: multi_object_id,
            oid: multi_oid,
            array_oid: multi_array_oid,
            array_name: multi_array.name,
            identity: multi,
        },
    };
    context.shells.complete(shell.as_ref())?;
    let publication = types.registries.ranges;
    definition.constructor_oids = Some(crate::routines::registration::register_range_constructors(
        &context.routines,
        &definition,
    )?);
    definition.cast_oid = Some(allocate(CatalogOidClass::Cast, &object_id)?);
    let before = publication.range_registry().clone();
    let mut registry = before.clone();
    registry.insert(name, definition)?;
    range_type::publish(publication, &before, registry)?;
    types.changes.catalog_registry_changed();
    Ok(())
}
