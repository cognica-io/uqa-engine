//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared names and OIDs for implemented virtual catalog relations.

macro_rules! virtual_relations {
    ($($variant:ident => ($schema:literal, $name:literal, $oid:expr)),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum VirtualRelation { $($variant),* }

        impl VirtualRelation {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            pub const fn namespace(self) -> &'static str {
                match self { $(Self::$variant => $schema),* }
            }

            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => $name),* }
            }

            pub fn oid(self) -> i64 {
                match self { $(Self::$variant => $oid),* }
            }

            /// Resolve decoded identifiers exactly; parser-owned case folding must not change quoted names.
            pub fn at(schema: &str, name: &str) -> Option<Self> {
                match (schema, name) { $(($schema, $name) => Some(Self::$variant),)* _ => None }
            }
        }
    };
}

virtual_relations! {
    InformationSchemaCatalogName => ("information_schema", "information_schema_catalog_name", 13313),
    InformationSchemata => ("information_schema", "schemata", 13467),
    InformationTables => ("information_schema", "tables", 13510),
    InformationColumns => ("information_schema", "columns", 13381),
    InformationColumnPrivileges => ("information_schema", "column_privileges", 13371),
    InformationRoleColumnGrants => ("information_schema", "role_column_grants", 13429),
    InformationViews => ("information_schema", "views", 13568),
    InformationRoutines => ("information_schema", "routines", 13462),
    InformationSequences => ("information_schema", "sequences", 13471),
    InformationTableConstraints => ("information_schema", "table_constraints", 13496),
    InformationKeyColumnUsage => ("information_schema", "key_column_usage", 13414),
    PgNamespace => ("pg_catalog", "pg_namespace", 2615),
    PgClass => ("pg_catalog", "pg_class", 1259),
    PgInherits => ("pg_catalog", "pg_inherits", 2611),
    PgPartitionedTable => ("pg_catalog", "pg_partitioned_table", 3350),
    PgAttribute => ("pg_catalog", "pg_attribute", 1249),
    PgAttrdef => ("pg_catalog", "pg_attrdef", 2604),
    PgConstraint => ("pg_catalog", "pg_constraint", 2606),
    PgIndex => ("pg_catalog", "pg_index", 2610),
    PgTrigger => ("pg_catalog", "pg_trigger", 2620),
    PgRewrite => ("pg_catalog", "pg_rewrite", 2618),
    PgRules => ("pg_catalog", "pg_rules", 12023),
    PgTables => ("pg_catalog", "pg_tables", 12033),
    PgViews => ("pg_catalog", "pg_views", 12028),
    PgIndexes => ("pg_catalog", "pg_indexes", 12043),
    PgType => ("pg_catalog", "pg_type", 1247),
    PgRange => ("pg_catalog", "pg_range", 3541),
    PgProc => ("pg_catalog", "pg_proc", 1255),
    PgDatabase => ("pg_catalog", "pg_database", 1262),
    PgAuthid => ("pg_catalog", "pg_authid", 1260),
    PgAuthMembers => ("pg_catalog", "pg_auth_members", 1261),
    PgRoles => ("pg_catalog", "pg_roles", 12000),
    PgUser => ("pg_catalog", "pg_user", 12014),
    PgSettings => ("pg_catalog", "pg_settings", 12104),
    PgPreparedStatements => ("pg_catalog", "pg_prepared_statements", 12095),
    PgCursors => ("pg_catalog", "pg_cursors", 12077),
    PgDescription => ("pg_catalog", "pg_description", 2609),
    PgMatviews => ("pg_catalog", "pg_matviews", 12038),
    PgSequences => ("pg_catalog", "pg_sequences", 12048),
    AgGraph => ("ag_catalog", "ag_graph", super::oids::relation_oid("relation", "ag_catalog", "ag_graph")),
    AgLabel => ("ag_catalog", "ag_label", super::oids::relation_oid("relation", "ag_catalog", "ag_label")),
}

impl VirtualRelation {
    pub fn qualified_name(self) -> String {
        format!("{}.{}", self.namespace(), self.name())
    }

    pub fn from_qualified_name(name: &str) -> Option<Self> {
        let (schema, local) = uqa_core::RelationIdentity::parse_reference(name).ok()?;
        Self::at(schema.as_deref()?, &local)
    }

    pub const fn kind(self) -> &'static str {
        if self.accepts_row_lock() {
            "table"
        } else {
            "view"
        }
    }
}

/// Resolve SQL spelling among virtual definitions. Catalog-aware consumers must also stop at earlier physical relations in the effective namespace.
pub fn resolve_virtual_relation(search_path: &[String], name: &str) -> Option<VirtualRelation> {
    let names = crate::parse_regobject_name(name)?;
    match names.as_slice() {
        [schema, name] => VirtualRelation::at(schema, name),
        [name] => {
            if !search_path.iter().any(|schema| schema == "pg_catalog") {
                if let Some(relation) = VirtualRelation::at("pg_catalog", name) {
                    return Some(relation);
                }
            }
            search_path
                .iter()
                .find_map(|schema| VirtualRelation::at(schema, name))
        }
        _ => None,
    }
}
