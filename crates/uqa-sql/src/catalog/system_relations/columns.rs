//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public attribute order for built-in relation authorization.

use super::SystemRelation;

impl SystemRelation {
    pub fn column_names(self) -> Vec<String> {
        let names: &[&str] = match self {
            Self::Projected(relation) => {
                return relation
                    .schema()
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect()
            }
            Self::PgAuthid => &[
                "oid",
                "rolname",
                "rolsuper",
                "rolinherit",
                "rolcreaterole",
                "rolcreatedb",
                "rolcanlogin",
                "rolreplication",
                "rolbypassrls",
                "rolconnlimit",
                "rolpassword",
                "rolvaliduntil",
            ],
            Self::PgDbRoleSetting => &["setdatabase", "setrole", "setconfig"],
            Self::PgShadow => &[
                "usename",
                "usesysid",
                "usecreatedb",
                "usesuper",
                "userepl",
                "usebypassrls",
                "passwd",
                "valuntil",
                "useconfig",
            ],
            Self::PgTablespace => &["oid", "spcname", "spcowner", "spcacl", "spcoptions"],
            Self::PgCollation => &[
                "oid",
                "collname",
                "collnamespace",
                "collowner",
                "collprovider",
                "collisdeterministic",
                "collencoding",
                "collcollate",
                "collctype",
                "colllocale",
                "collicurules",
                "collversion",
            ],
            Self::PgDepend => &[
                "classid",
                "objid",
                "objsubid",
                "refclassid",
                "refobjid",
                "refobjsubid",
                "deptype",
            ],
            Self::PgSequence => &[
                "seqrelid",
                "seqtypid",
                "seqstart",
                "seqincrement",
                "seqmax",
                "seqmin",
                "seqcache",
                "seqcycle",
            ],
            Self::PgLanguage => &[
                "oid",
                "lanname",
                "lanowner",
                "lanispl",
                "lanpltrusted",
                "lanplcallfoid",
                "laninline",
                "lanvalidator",
                "lanacl",
            ],
            Self::InformationEnabledRoles => &["role_name"],
        };
        names.iter().map(|name| (*name).into()).collect()
    }
}
