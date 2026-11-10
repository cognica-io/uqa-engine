//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native type references resolve against admitted identities without rebinding names.

use super::{
    composite_type::StoredComposite, domain::StoredDomain, enum_type::StoredEnum,
    range_type::StoredRange, relation_oids::RelationCatalogOids, shell_type::StoredShellType,
    type_definitions::TypeDefinitions, type_metadata,
};
use crate::{
    ast::{ColumnType, RangeAlignment, UserTypeIdentity, UserTypeKind},
    SQLError,
};
use uqa_core::{
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken,
};

/// The original selected type registries; relation owners supply their row-type identities separately.
pub struct TypeReferenceInput<'a> {
    pub domains: &'a TypeDefinitions<StoredDomain>,
    pub enums: &'a TypeDefinitions<StoredEnum>,
    pub composites: &'a TypeDefinitions<StoredComposite>,
    pub ranges: &'a TypeDefinitions<StoredRange>,
    pub shells: &'a TypeDefinitions<StoredShellType>,
}

struct Claim<'a> {
    kind: Option<UserTypeKind>,
    array: u32,
    relation: u32,
    domain: Option<&'a StoredDomain>,
    alignment: Option<RangeAlignment>,
}

impl Claim<'_> {
    fn new(kind: Option<UserTypeKind>, array: u32) -> Self {
        Self {
            kind,
            array,
            relation: 0,
            domain: None,
            alignment: None,
        }
    }
}

/// Complete OID claims are installed before references are checked, allowing forward references while rejecting cross-family collisions. All payloads remain borrowed.
pub struct TypeReferences<'a> {
    claims: BudgetedMap<u32, Claim<'a>>,
    memory: &'a MemoryBudget,
    cancellation: &'a CancellationToken,
}

impl<'a> TypeReferences<'a> {
    pub fn new(
        input: TypeReferenceInput<'a>,
        rows: impl Iterator<Item = RelationCatalogOids>,
        memory: &'a MemoryBudget,
        cancellation: &'a CancellationToken,
    ) -> Result<Self, SQLError> {
        let mut result = Self {
            claims: BudgetedMap::new(memory),
            memory,
            cancellation,
        };
        cancellation.check()?;
        for domain in input.domains.values() {
            let array = u32::try_from(type_metadata::pg_domain_array_oid(
                domain.oid,
                domain.array_oid,
            ))
            .map_err(|_| invalid("domain array identity"))?;
            let mut claim = Claim::new(Some(UserTypeKind::Domain), array);
            claim.domain = Some(domain);
            result.insert(domain.oid, claim)?;
        }
        for definition in input.enums.values() {
            result.insert(
                definition.oid,
                Claim::new(Some(UserTypeKind::Enum), definition.array_oid),
            )?;
        }
        for definition in input.composites.values() {
            let mut claim = Claim::new(Some(UserTypeKind::Composite), definition.array_oid);
            claim.relation = definition.relation_oid;
            result.insert(definition.oid, claim)?;
        }
        for definition in input.ranges.values() {
            for (oid, array, kind) in [
                (definition.oid, definition.array_oid, UserTypeKind::Range),
                (
                    definition.multirange.oid,
                    definition.multirange.array_oid,
                    UserTypeKind::Multirange,
                ),
            ] {
                let mut claim = Claim::new(Some(kind), array);
                claim.alignment = Some(result.endpoint_alignment(&definition.subtype)?);
                result.insert(oid, claim)?;
            }
        }
        for definition in input.shells.values() {
            result.insert(definition.oid, Claim::new(None, 0))?;
        }
        for row in rows {
            cancellation.check()?;
            let Some(oid) = row.row_type else {
                return Err(invalid("relation has no row type"));
            };
            let mut claim = Claim::new(Some(UserTypeKind::Composite), row.array_type.unwrap_or(0));
            claim.relation = row.relation;
            result.insert(oid, claim)?;
        }
        Ok(result)
    }

    fn insert(&mut self, oid: u32, claim: Claim<'a>) -> Result<(), SQLError> {
        let array = claim.array;
        self.insert_one(oid, claim)?;
        if array != 0 {
            self.insert_one(array, Claim::new(None, 0))?;
        }
        Ok(())
    }

    fn insert_one(&mut self, oid: u32, claim: Claim<'a>) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if oid == 0
            || type_metadata::builtin_scalar_type(oid).is_some()
            || type_metadata::builtin_array_element(oid).is_some()
            || type_metadata::routine_pseudo_type_name(i64::from(oid)).is_some()
            || super::is_system_catalog_type_oid(oid)
            || self.claims.insert(oid, claim)?.is_some()
        {
            return Err(invalid("duplicate or reserved type OID"));
        }
        Ok(())
    }

    fn control(&self) -> ProductionControl<'_> {
        ProductionControl::new(self.memory, self.cancellation, self.cancellation)
    }

    fn claim(&self, oid: u32, kind: UserTypeKind) -> Result<&Claim<'a>, SQLError> {
        self.cancellation.check()?;
        self.claims
            .get(&oid)
            .filter(|claim| claim.kind == Some(kind))
            .ok_or_else(|| invalid("missing type or different type kind"))
    }

    /// Stored names representing a bound user type resolve by kind and OID, independently of display names or search paths. Ordinary builtin and pseudo-type spellings retain their existing declaration validation.
    pub fn stored_name(&self, name: &str) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if let Some(identity) = UserTypeIdentity::parse(name) {
            if identity.kind == UserTypeKind::Domain
                && super::system_catalog_domain_with_control(identity.oid, &self.control())?
                    .is_some()
            {
                return Ok(());
            }
            self.claim(identity.oid, identity.kind)?;
        }
        if ["enum#", "domain#", "composite#", "range#", "multirange#"]
            .iter()
            .any(|prefix| name.trim().starts_with(prefix))
            && UserTypeIdentity::parse(name).is_none()
        {
            return Err(invalid("malformed stored type identity"));
        }
        Ok(())
    }

    /// Check the physical identity fields carried with a concrete type, including nested arrays and domain bases. Reference names are descriptive and never replace the selected OID.
    pub fn column_type(&self, mut ty: &ColumnType) -> Result<(), SQLError> {
        loop {
            self.cancellation.check()?;
            match ty {
                ColumnType::Named(_) => return Err(invalid("unresolved column type")),
                ColumnType::Array(element) => ty = element,
                ColumnType::Domain {
                    oid,
                    array_oid,
                    collation_oid,
                    base,
                    ..
                } => {
                    if let Some(expected) =
                        super::system_catalog_domain_with_control(*oid, &self.control())?
                    {
                        let ColumnType::Domain {
                            array_oid: expected_array,
                            collation_oid: expected_collation,
                            base: expected_base,
                            ..
                        } = &*expected
                        else {
                            unreachable!("catalog domain projection");
                        };
                        self.domain_base(
                            (*oid, *array_oid, *collation_oid, base),
                            (*expected_array, *expected_collation, expected_base),
                        )?;
                    } else {
                        let definition = self
                            .claim(*oid, UserTypeKind::Domain)?
                            .domain
                            .expect("domain claim");
                        self.domain_base(
                            (*oid, *array_oid, *collation_oid, base),
                            (
                                definition.array_oid,
                                definition.definition.collation_oid,
                                &definition.definition.base,
                            ),
                        )?;
                    }
                    ty = base;
                }
                ColumnType::Enum(reference) => {
                    if self.claim(reference.oid, UserTypeKind::Enum)?.array != reference.array_oid {
                        return Err(invalid("enum array identity"));
                    }
                    return Ok(());
                }
                ColumnType::Composite(reference) => {
                    let claim = self.claim(reference.oid, UserTypeKind::Composite)?;
                    if claim.array != reference.array_oid
                        || claim.relation != reference.relation_oid
                    {
                        return Err(invalid("composite relation or array identity"));
                    }
                    return Ok(());
                }
                ColumnType::DeclaredRange(reference)
                | ColumnType::DeclaredMultirange(reference) => {
                    let kind = if matches!(ty, ColumnType::DeclaredRange(_)) {
                        UserTypeKind::Range
                    } else {
                        UserTypeKind::Multirange
                    };
                    let claim = self.claim(reference.oid, kind)?;
                    if claim.array != reference.array_oid
                        || claim.alignment != Some(reference.alignment)
                    {
                        return Err(invalid("range array identity or alignment"));
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }
    }

    fn domain_base(
        &self,
        (oid, array, collation, base): (u32, Option<u32>, Option<u32>, &ColumnType),
        (expected_array, expected_collation, expected_base): (
            Option<u32>,
            Option<u32>,
            &ColumnType,
        ),
    ) -> Result<(), SQLError> {
        self.cancellation.check()?;
        if type_metadata::pg_domain_array_oid(oid, array)
            != type_metadata::pg_domain_array_oid(oid, expected_array)
            || collation != expected_collation
            || self.physical_identity(base)? != self.physical_identity(expected_base)?
        {
            return Err(invalid("domain base or physical identity"));
        }
        Ok(())
    }

    fn physical_identity(&self, mut ty: &ColumnType) -> Result<(i64, i64), SQLError> {
        let array = matches!(ty, ColumnType::Array(_));
        while let ColumnType::Array(element) = ty {
            self.cancellation.check()?;
            ty = element;
        }
        if matches!(ty, ColumnType::Named(_)) {
            return Err(invalid("unresolved domain base"));
        }
        let oid = if array {
            type_metadata::pg_type_array_oid(ty)
        } else {
            type_metadata::pg_type_oid(ty)
        };
        Ok((oid, type_metadata::pg_type_modifier(ty)))
    }

    fn endpoint_alignment(&self, mut ty: &ColumnType) -> Result<RangeAlignment, SQLError> {
        while let ColumnType::Domain { base, .. } = ty {
            self.cancellation.check()?;
            ty = base;
        }
        if matches!(ty, ColumnType::Named(_)) {
            return Err(invalid("unresolved range endpoint"));
        }
        Ok(RangeAlignment::for_endpoint(ty))
    }
}

fn invalid(reason: &'static str) -> SQLError {
    SQLError::Internal(format!("invalid native type reference: {reason}"))
}

mod membership;

#[cfg(test)]
mod tests;
