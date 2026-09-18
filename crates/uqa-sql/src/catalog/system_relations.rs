//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Built-in relation identity, bootstrap security and ordered view references.

use super::{
    security::{TableAclEntry, TablePrivileges, TableSecurity},
    VirtualRelation,
};

macro_rules! system_relations {
    ($($variant:ident => ($schema:literal, $name:literal, $oid:literal, $kind:literal)),* $(,)?) => {
        /// Catalog identity is independent of whether a virtual row projector is available.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum SystemRelation {
            Projected(VirtualRelation),
            $($variant),*
        }

        impl SystemRelation {
            pub fn all() -> impl Iterator<Item = Self> {
                VirtualRelation::ALL.iter().copied().map(Self::Projected)
                    .chain([$(Self::$variant),*])
            }

            pub const fn namespace(self) -> &'static str {
                match self {
                    Self::Projected(relation) => relation.namespace(),
                    $(Self::$variant => $schema),*
                }
            }

            pub const fn name(self) -> &'static str {
                match self {
                    Self::Projected(relation) => relation.name(),
                    $(Self::$variant => $name),*
                }
            }

            pub fn oid(self) -> i64 {
                match self {
                    Self::Projected(relation) => relation.oid(),
                    $(Self::$variant => $oid),*
                }
            }

            pub const fn kind(self) -> &'static str {
                match self {
                    Self::Projected(relation) => relation.kind(),
                    $(Self::$variant => $kind),*
                }
            }

            pub fn at(schema: &str, name: &str) -> Option<Self> {
                VirtualRelation::at(schema, name).map(Self::Projected).or_else(|| {
                    match (schema, name) {
                        $(($schema, $name) => Some(Self::$variant),)*
                        _ => None,
                    }
                })
            }
        }
    };
}

system_relations! {
    PgAuthid => ("pg_catalog", "pg_authid", 1260, "table"),
    PgDbRoleSetting => ("pg_catalog", "pg_db_role_setting", 2964, "table"),
    PgShadow => ("pg_catalog", "pg_shadow", 12005, "view"),
    PgTablespace => ("pg_catalog", "pg_tablespace", 1213, "table"),
    PgCollation => ("pg_catalog", "pg_collation", 3456, "table"),
    PgDepend => ("pg_catalog", "pg_depend", 2608, "table"),
    PgSequence => ("pg_catalog", "pg_sequence", 2224, "table"),
    PgLanguage => ("pg_catalog", "pg_language", 2612, "table"),
    InformationEnabledRoles => ("information_schema", "enabled_roles", 13410, "view"),
}

impl SystemRelation {
    pub fn qualified_name(self) -> String {
        format!("{}.{}", self.namespace(), self.name())
    }

    pub fn from_qualified_name(name: &str) -> Option<Self> {
        let (schema, local) = uqa_core::RelationIdentity::parse_reference(name).ok()?;
        Self::at(schema.as_deref()?, &local)
    }

    /// Immutable built-ins retain the same identity in every session; lock managers retain database affinity.
    pub fn object_id(self) -> [u8; 16] {
        let mut identity = [0; 16];
        identity[..8].copy_from_slice(b"UQA:cat:");
        identity[8..].copy_from_slice(&self.oid().to_be_bytes());
        identity
    }

    pub fn bootstrap_security(self) -> TableSecurity {
        let mut security = TableSecurity::owner(super::oids::current_user_name());
        let mut acl = vec![TableAclEntry {
            role: security.role_owner.clone(),
            grantor: Some(security.role_owner.clone()),
            privileges: TablePrivileges::ALL,
            grant_options: TablePrivileges::default(),
        }];
        if !matches!(
            self,
            Self::PgAuthid
                | Self::PgShadow
                | Self::Projected(VirtualRelation::AgGraph | VirtualRelation::AgLabel)
        ) {
            acl.push(TableAclEntry {
                role: "PUBLIC".into(),
                grantor: Some(security.role_owner.clone()),
                privileges: TablePrivileges {
                    select: true,
                    update: self == Self::Projected(VirtualRelation::PgSettings),
                    ..TablePrivileges::default()
                },
                grant_options: TablePrivileges::default(),
            });
        }
        security.acl = Some(acl);
        security
    }

    /// `PostgreSQL` 18 view range-table order, followed by expression and range-table subqueries. Repeated references preserve that order; consumers own recursion and lock retention.
    pub const fn view_sources(self) -> &'static [Self] {
        use SystemRelation::Projected as P;
        use VirtualRelation::{
            InformationColumnPrivileges, PgAttrdef, PgAttribute, PgClass, PgConstraint, PgIndex,
            PgNamespace, PgProc, PgRewrite, PgTrigger, PgType,
        };
        match self {
            P(VirtualRelation::InformationSchemata) => &[P(PgNamespace), Self::PgAuthid],
            P(VirtualRelation::InformationTables) => {
                &[P(PgNamespace), P(PgClass), P(PgType), P(PgNamespace)]
            }
            P(VirtualRelation::InformationColumns) => &[
                P(PgAttribute),
                P(PgAttrdef),
                P(PgClass),
                P(PgNamespace),
                P(PgType),
                P(PgNamespace),
                P(PgType),
                P(PgNamespace),
                Self::PgCollation,
                P(PgNamespace),
                Self::PgDepend,
                Self::PgSequence,
            ],
            P(InformationColumnPrivileges) => &[
                P(PgNamespace),
                Self::PgAuthid,
                P(PgAttribute),
                P(PgClass),
                P(PgClass),
                P(PgAttribute),
                P(PgClass),
                Self::PgAuthid,
            ],
            P(VirtualRelation::InformationRoleColumnGrants) => &[
                P(InformationColumnPrivileges),
                Self::InformationEnabledRoles,
                Self::InformationEnabledRoles,
            ],
            P(VirtualRelation::InformationViews) => &[
                P(PgNamespace),
                P(PgClass),
                P(PgTrigger),
                P(PgTrigger),
                P(PgTrigger),
            ],
            P(VirtualRelation::InformationRoutines) => &[
                P(PgNamespace),
                P(PgProc),
                Self::PgLanguage,
                P(PgType),
                P(PgNamespace),
            ],
            P(VirtualRelation::InformationSequences) => {
                &[P(PgNamespace), P(PgClass), Self::PgSequence, Self::PgDepend]
            }
            P(VirtualRelation::InformationTableConstraints) => &[
                P(PgNamespace),
                P(PgNamespace),
                P(PgConstraint),
                P(PgClass),
                P(PgIndex),
            ],
            P(VirtualRelation::InformationKeyColumnUsage) => &[
                P(PgAttribute),
                P(PgNamespace),
                P(PgClass),
                P(PgNamespace),
                P(PgConstraint),
            ],
            P(VirtualRelation::PgRules) => &[P(PgRewrite), P(PgClass), P(PgNamespace)],
            P(VirtualRelation::PgTables | VirtualRelation::PgMatviews) => {
                &[P(PgClass), P(PgNamespace), Self::PgTablespace]
            }
            P(VirtualRelation::PgViews) => &[P(PgClass), P(PgNamespace)],
            P(VirtualRelation::PgIndexes) => &[
                P(PgIndex),
                P(PgClass),
                P(PgClass),
                P(PgNamespace),
                Self::PgTablespace,
            ],
            P(VirtualRelation::PgRoles) | Self::PgShadow => {
                &[Self::PgAuthid, Self::PgDbRoleSetting]
            }
            P(VirtualRelation::PgUser) => &[Self::PgShadow],
            P(VirtualRelation::PgSequences) => &[Self::PgSequence, P(PgClass), P(PgNamespace)],
            Self::InformationEnabledRoles => &[Self::PgAuthid],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests;
