//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public attribute order for built-in relation authorization.

use super::SystemRelation;

impl SystemRelation {
    pub fn column_names(self) -> Vec<String> {
        self.column_names_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary system column production")
            .into_uncontrolled()
            .expect("ordinary columns")
    }

    pub fn column_names_with_control(
        self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, crate::SQLError> {
        control.check()?;
        let names: &[&str] = match self {
            Self::Projected(relation) => return relation.column_names_with_control(control),
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
            Self::InformationEnabledRoles => &["role_name"],
        };
        let mut output = uqa_core::memory::ProductionVec::new(*control);
        for name in names {
            output.push_produced(control.copy_text(name)?)?;
        }
        Ok(output.finish()?)
    }
}
