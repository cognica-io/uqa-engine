//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 row types for the virtual system-catalog relations implemented by the engine.

use crate::ast::ColumnType;
mod schema_production;
use schema_production::CatalogColumnType::{self, Array, Domain, Inline};

pub mod access_methods;
pub mod collations;
pub mod expression_text;
pub mod foreign_relations;
pub mod languages;
pub mod node_tree;
pub mod oids;
pub mod resolution;
pub mod result_type;
pub mod type_metadata;
mod type_production;
pub use type_production::system_catalog_domain_with_control;

/// Namespace of the Apache AGE catalog relations, types, and functions.
pub const AG_CATALOG_SCHEMA: &str = "ag_catalog";

mod virtual_relations;
pub use virtual_relations::{resolve_virtual_relation, VirtualRelation};
mod system_relations;
pub use system_relations::SystemRelation;

impl VirtualRelation {
    pub const fn accepts_row_lock(self) -> bool {
        matches!(
            self,
            Self::PgNamespace
                | Self::PgClass
                | Self::PgInherits
                | Self::PgPartitionedTable
                | Self::PgAttribute
                | Self::PgAttrdef
                | Self::PgConstraint
                | Self::PgIndex
                | Self::PgTrigger
                | Self::PgRewrite
                | Self::PgType
                | Self::PgRange
                | Self::PgEnum
                | Self::PgProc
                | Self::PgLanguage
                | Self::PgExtension
                | Self::PgForeignDataWrapper
                | Self::PgForeignServer
                | Self::PgForeignTable
                | Self::PgDatabase
                | Self::PgAuthid
                | Self::PgAuthMembers
                | Self::PgDescription
                | Self::PgDepend
                | Self::PgShdepend
                | Self::AgGraph
                | Self::AgLabel
        )
    }

    pub fn schema(self) -> Vec<(String, ColumnType)> {
        self.schema_with_control(&uqa_core::memory::ProductionControl::uncontrolled())
            .expect("ordinary catalog schema")
            .into_uncontrolled()
            .expect("ordinary catalog owner")
    }

    pub fn column_names_with_control(
        self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, crate::SQLError> {
        control.check()?;
        let mut names = uqa_core::memory::ProductionVec::new(*control);
        let mut status = Ok(());
        self.visit_columns(&mut |name, _| {
            if status.is_ok() {
                status = control
                    .copy_text(name)
                    .and_then(|name| names.push_produced(name));
            }
        });
        status?;
        Ok(names.finish()?)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves catalog column and OID order"
    )]
    fn visit_columns(self, visit: &mut dyn FnMut(&'static str, CatalogColumnType)) {
        // Each relation owns its construction frame, keeping unoptimized WASM locals bounded.
        macro_rules! columns {
            ($($name:literal => $ty:expr),* $(,)?) => {{
                fn relation_columns(visit: &mut dyn FnMut(&'static str, CatalogColumnType)) {
                    $(visit($name, $ty);)*
                }
                relation_columns(visit)
            }};
        }

        match self {
            Self::PgForeignDataWrapper | Self::PgForeignServer | Self::PgForeignTable => {
                foreign_relations::visit_columns(self, visit);
            }
            Self::InformationSchemaCatalogName => columns![
                "catalog_name" => Domain(13312),
            ],
            Self::InformationSchemata => columns![
                "catalog_name" => Domain(13312),
                "schema_name" => Domain(13312),
                "schema_owner" => Domain(13312),
                "default_character_set_catalog" => Domain(13312),
                "default_character_set_schema" => Domain(13312),
                "default_character_set_name" => Domain(13312),
                "sql_path" => Domain(13310),
            ],
            Self::InformationTables => columns![
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "table_type" => Domain(13310),
                "self_referencing_column_name" => Domain(13312),
                "reference_generation" => Domain(13310),
                "user_defined_type_catalog" => Domain(13312),
                "user_defined_type_schema" => Domain(13312),
                "user_defined_type_name" => Domain(13312),
                "is_insertable_into" => Domain(13320),
                "is_typed" => Domain(13320),
                "commit_action" => Domain(13310),
            ],
            Self::InformationColumns => columns![
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "column_name" => Domain(13312),
                "ordinal_position" => Domain(13307),
                "column_default" => Domain(13310),
                "is_nullable" => Domain(13320),
                "data_type" => Domain(13310),
                "character_maximum_length" => Domain(13307),
                "character_octet_length" => Domain(13307),
                "numeric_precision" => Domain(13307),
                "numeric_precision_radix" => Domain(13307),
                "numeric_scale" => Domain(13307),
                "datetime_precision" => Domain(13307),
                "interval_type" => Domain(13310),
                "interval_precision" => Domain(13307),
                "character_set_catalog" => Domain(13312),
                "character_set_schema" => Domain(13312),
                "character_set_name" => Domain(13312),
                "collation_catalog" => Domain(13312),
                "collation_schema" => Domain(13312),
                "collation_name" => Domain(13312),
                "domain_catalog" => Domain(13312),
                "domain_schema" => Domain(13312),
                "domain_name" => Domain(13312),
                "udt_catalog" => Domain(13312),
                "udt_schema" => Domain(13312),
                "udt_name" => Domain(13312),
                "scope_catalog" => Domain(13312),
                "scope_schema" => Domain(13312),
                "scope_name" => Domain(13312),
                "maximum_cardinality" => Domain(13307),
                "dtd_identifier" => Domain(13312),
                "is_self_referencing" => Domain(13320),
                "is_identity" => Domain(13320),
                "identity_generation" => Domain(13310),
                "identity_start" => Domain(13310),
                "identity_increment" => Domain(13310),
                "identity_maximum" => Domain(13310),
                "identity_minimum" => Domain(13310),
                "identity_cycle" => Domain(13320),
                "is_generated" => Domain(13310),
                "generation_expression" => Domain(13310),
                "is_updatable" => Domain(13320),
            ],
            Self::InformationColumnPrivileges | Self::InformationRoleColumnGrants => columns![
                "grantor" => Domain(13312),
                "grantee" => Domain(13312),
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "column_name" => Domain(13312),
                "privilege_type" => Domain(13310),
                "is_grantable" => Domain(13320),
            ],
            Self::InformationViews => columns![
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "view_definition" => Domain(13310),
                "check_option" => Domain(13310),
                "is_updatable" => Domain(13320),
                "is_insertable_into" => Domain(13320),
                "is_trigger_updatable" => Domain(13320),
                "is_trigger_deletable" => Domain(13320),
                "is_trigger_insertable_into" => Domain(13320),
            ],
            Self::InformationRoutines => information_routines_columns(visit),
            Self::InformationSequences => columns![
                "sequence_catalog" => Domain(13312),
                "sequence_schema" => Domain(13312),
                "sequence_name" => Domain(13312),
                "data_type" => Domain(13310),
                "numeric_precision" => Domain(13307),
                "numeric_precision_radix" => Domain(13307),
                "numeric_scale" => Domain(13307),
                "start_value" => Domain(13310),
                "minimum_value" => Domain(13310),
                "maximum_value" => Domain(13310),
                "increment" => Domain(13310),
                "cycle_option" => Domain(13320),
            ],
            Self::InformationTableConstraints => columns![
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "constraint_type" => Domain(13310),
                "is_deferrable" => Domain(13320),
                "initially_deferred" => Domain(13320),
                "enforced" => Domain(13320),
                "nulls_distinct" => Domain(13320),
            ],
            Self::InformationCheckConstraints => columns![
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
                "check_clause" => Domain(13310),
            ],
            Self::InformationDomainConstraints => columns![
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
                "domain_catalog" => Domain(13312),
                "domain_schema" => Domain(13312),
                "domain_name" => Domain(13312),
                "is_deferrable" => Domain(13320),
                "initially_deferred" => Domain(13320),
            ],
            Self::InformationKeyColumnUsage => columns![
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "column_name" => Domain(13312),
                "ordinal_position" => Domain(13307),
                "position_in_unique_constraint" => Domain(13307),
            ],
            Self::InformationConstraintColumnUsage => columns![
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "column_name" => Domain(13312),
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
            ],
            Self::InformationConstraintTableUsage => columns![
                "table_catalog" => Domain(13312),
                "table_schema" => Domain(13312),
                "table_name" => Domain(13312),
                "constraint_catalog" => Domain(13312),
                "constraint_schema" => Domain(13312),
                "constraint_name" => Domain(13312),
            ],
            Self::PgNamespace => columns![
                "oid" => Inline(ColumnType::Oid),
                "nspname" => Inline(ColumnType::Name),
                "nspowner" => Inline(ColumnType::Oid),
                "nspacl" => Array(ColumnType::AclItem),
            ],
            Self::PgClass => columns![
                "oid" => Inline(ColumnType::Oid),
                "relname" => Inline(ColumnType::Name),
                "relnamespace" => Inline(ColumnType::Oid),
                "reltype" => Inline(ColumnType::Oid),
                "reloftype" => Inline(ColumnType::Oid),
                "relowner" => Inline(ColumnType::Oid),
                "relam" => Inline(ColumnType::Oid),
                "relfilenode" => Inline(ColumnType::Oid),
                "reltablespace" => Inline(ColumnType::Oid),
                "relpages" => Inline(ColumnType::Integer),
                "reltuples" => Inline(ColumnType::Real),
                "relallvisible" => Inline(ColumnType::Integer),
                "relallfrozen" => Inline(ColumnType::Integer),
                "reltoastrelid" => Inline(ColumnType::Oid),
                "relhasindex" => Inline(ColumnType::Boolean),
                "relisshared" => Inline(ColumnType::Boolean),
                "relpersistence" => Inline(ColumnType::InternalChar),
                "relkind" => Inline(ColumnType::InternalChar),
                "relnatts" => Inline(ColumnType::SmallInteger),
                "relchecks" => Inline(ColumnType::SmallInteger),
                "relhasrules" => Inline(ColumnType::Boolean),
                "relhastriggers" => Inline(ColumnType::Boolean),
                "relhassubclass" => Inline(ColumnType::Boolean),
                "relrowsecurity" => Inline(ColumnType::Boolean),
                "relforcerowsecurity" => Inline(ColumnType::Boolean),
                "relispopulated" => Inline(ColumnType::Boolean),
                "relreplident" => Inline(ColumnType::InternalChar),
                "relispartition" => Inline(ColumnType::Boolean),
                "relrewrite" => Inline(ColumnType::Oid),
                "relfrozenxid" => Inline(ColumnType::Xid),
                "relminmxid" => Inline(ColumnType::Xid),
                "relacl" => Array(ColumnType::AclItem),
                "reloptions" => Array(ColumnType::Text),
                "relpartbound" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgInherits => columns![
                "inhrelid" => Inline(ColumnType::Oid),
                "inhparent" => Inline(ColumnType::Oid),
                "inhseqno" => Inline(ColumnType::Integer),
                "inhdetachpending" => Inline(ColumnType::Boolean),
            ],
            Self::PgPartitionedTable => columns![
                "partrelid" => Inline(ColumnType::Oid),
                "partstrat" => Inline(ColumnType::InternalChar),
                "partnatts" => Inline(ColumnType::SmallInteger),
                "partdefid" => Inline(ColumnType::Oid),
                "partattrs" => Inline(ColumnType::Int2Vector),
                "partclass" => Inline(ColumnType::OidVector),
                "partcollation" => Inline(ColumnType::OidVector),
                "partexprs" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgAttribute => columns![
                "attrelid" => Inline(ColumnType::Oid),
                "attname" => Inline(ColumnType::Name),
                "atttypid" => Inline(ColumnType::Oid),
                "attlen" => Inline(ColumnType::SmallInteger),
                "attnum" => Inline(ColumnType::SmallInteger),
                "atttypmod" => Inline(ColumnType::Integer),
                "attndims" => Inline(ColumnType::SmallInteger),
                "attbyval" => Inline(ColumnType::Boolean),
                "attalign" => Inline(ColumnType::InternalChar),
                "attstorage" => Inline(ColumnType::InternalChar),
                "attcompression" => Inline(ColumnType::InternalChar),
                "attnotnull" => Inline(ColumnType::Boolean),
                "atthasdef" => Inline(ColumnType::Boolean),
                "atthasmissing" => Inline(ColumnType::Boolean),
                "attidentity" => Inline(ColumnType::InternalChar),
                "attgenerated" => Inline(ColumnType::InternalChar),
                "attisdropped" => Inline(ColumnType::Boolean),
                "attislocal" => Inline(ColumnType::Boolean),
                "attinhcount" => Inline(ColumnType::SmallInteger),
                "attcollation" => Inline(ColumnType::Oid),
                "attstattarget" => Inline(ColumnType::SmallInteger),
                "attacl" => Array(ColumnType::AclItem),
                "attoptions" => Array(ColumnType::Text),
                "attfdwoptions" => Array(ColumnType::Text),
                "attmissingval" => Inline(ColumnType::AnyArray),
            ],
            Self::PgAttrdef => columns![
                "oid" => Inline(ColumnType::Oid),
                "adrelid" => Inline(ColumnType::Oid),
                "adnum" => Inline(ColumnType::SmallInteger),
                "adbin" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgConstraint => columns![
                "oid" => Inline(ColumnType::Oid),
                "conname" => Inline(ColumnType::Name),
                "connamespace" => Inline(ColumnType::Oid),
                "contype" => Inline(ColumnType::InternalChar),
                "condeferrable" => Inline(ColumnType::Boolean),
                "condeferred" => Inline(ColumnType::Boolean),
                "conenforced" => Inline(ColumnType::Boolean),
                "convalidated" => Inline(ColumnType::Boolean),
                "conrelid" => Inline(ColumnType::Oid),
                "contypid" => Inline(ColumnType::Oid),
                "conindid" => Inline(ColumnType::Oid),
                "conparentid" => Inline(ColumnType::Oid),
                "confrelid" => Inline(ColumnType::Oid),
                "confupdtype" => Inline(ColumnType::InternalChar),
                "confdeltype" => Inline(ColumnType::InternalChar),
                "confmatchtype" => Inline(ColumnType::InternalChar),
                "conislocal" => Inline(ColumnType::Boolean),
                "coninhcount" => Inline(ColumnType::SmallInteger),
                "connoinherit" => Inline(ColumnType::Boolean),
                "conperiod" => Inline(ColumnType::Boolean),
                "conkey" => Array(ColumnType::SmallInteger),
                "confkey" => Array(ColumnType::SmallInteger),
                "conpfeqop" => Array(ColumnType::Oid),
                "conppeqop" => Array(ColumnType::Oid),
                "conffeqop" => Array(ColumnType::Oid),
                "confdelsetcols" => Array(ColumnType::SmallInteger),
                "conexclop" => Array(ColumnType::Oid),
                "conbin" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgIndex => columns![
                "indexrelid" => Inline(ColumnType::Oid),
                "indrelid" => Inline(ColumnType::Oid),
                "indnatts" => Inline(ColumnType::SmallInteger),
                "indnkeyatts" => Inline(ColumnType::SmallInteger),
                "indisunique" => Inline(ColumnType::Boolean),
                "indnullsnotdistinct" => Inline(ColumnType::Boolean),
                "indisprimary" => Inline(ColumnType::Boolean),
                "indisexclusion" => Inline(ColumnType::Boolean),
                "indimmediate" => Inline(ColumnType::Boolean),
                "indisclustered" => Inline(ColumnType::Boolean),
                "indisvalid" => Inline(ColumnType::Boolean),
                "indcheckxmin" => Inline(ColumnType::Boolean),
                "indisready" => Inline(ColumnType::Boolean),
                "indislive" => Inline(ColumnType::Boolean),
                "indisreplident" => Inline(ColumnType::Boolean),
                "indkey" => Inline(ColumnType::Int2Vector),
                "indcollation" => Inline(ColumnType::OidVector),
                "indclass" => Inline(ColumnType::OidVector),
                "indoption" => Inline(ColumnType::Int2Vector),
                "indexprs" => Inline(ColumnType::PgNodeTree),
                "indpred" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgTrigger => columns![
                "oid" => Inline(ColumnType::Oid),
                "tgrelid" => Inline(ColumnType::Oid),
                "tgparentid" => Inline(ColumnType::Oid),
                "tgname" => Inline(ColumnType::Name),
                "tgfoid" => Inline(ColumnType::Oid),
                "tgtype" => Inline(ColumnType::SmallInteger),
                "tgenabled" => Inline(ColumnType::InternalChar),
                "tgisinternal" => Inline(ColumnType::Boolean),
                "tgconstrrelid" => Inline(ColumnType::Oid),
                "tgconstrindid" => Inline(ColumnType::Oid),
                "tgconstraint" => Inline(ColumnType::Oid),
                "tgdeferrable" => Inline(ColumnType::Boolean),
                "tginitdeferred" => Inline(ColumnType::Boolean),
                "tgnargs" => Inline(ColumnType::SmallInteger),
                "tgattr" => Inline(ColumnType::Int2Vector),
                "tgargs" => Inline(ColumnType::Bytea),
                "tgqual" => Inline(ColumnType::PgNodeTree),
                "tgoldtable" => Inline(ColumnType::Name),
                "tgnewtable" => Inline(ColumnType::Name),
            ],
            Self::PgRewrite => columns![
                "oid" => Inline(ColumnType::Oid),
                "rulename" => Inline(ColumnType::Name),
                "ev_class" => Inline(ColumnType::Oid),
                "ev_type" => Inline(ColumnType::InternalChar),
                "ev_enabled" => Inline(ColumnType::InternalChar),
                "is_instead" => Inline(ColumnType::Boolean),
                "ev_qual" => Inline(ColumnType::PgNodeTree),
                "ev_action" => Inline(ColumnType::PgNodeTree),
            ],
            Self::PgRules => columns![
                "schemaname" => Inline(ColumnType::Name),
                "tablename" => Inline(ColumnType::Name),
                "rulename" => Inline(ColumnType::Name),
                "definition" => Inline(ColumnType::Text),
            ],
            Self::PgTables => columns![
                "schemaname" => Inline(ColumnType::Name),
                "tablename" => Inline(ColumnType::Name),
                "tableowner" => Inline(ColumnType::Name),
                "tablespace" => Inline(ColumnType::Name),
                "hasindexes" => Inline(ColumnType::Boolean),
                "hasrules" => Inline(ColumnType::Boolean),
                "hastriggers" => Inline(ColumnType::Boolean),
                "rowsecurity" => Inline(ColumnType::Boolean),
            ],
            Self::PgViews => columns![
                "schemaname" => Inline(ColumnType::Name),
                "viewname" => Inline(ColumnType::Name),
                "viewowner" => Inline(ColumnType::Name),
                "definition" => Inline(ColumnType::Text),
            ],
            Self::PgIndexes => columns![
                "schemaname" => Inline(ColumnType::Name),
                "tablename" => Inline(ColumnType::Name),
                "indexname" => Inline(ColumnType::Name),
                "tablespace" => Inline(ColumnType::Name),
                "indexdef" => Inline(ColumnType::Text),
            ],
            Self::PgType => columns![
                "oid" => Inline(ColumnType::Oid),
                "typname" => Inline(ColumnType::Name),
                "typnamespace" => Inline(ColumnType::Oid),
                "typowner" => Inline(ColumnType::Oid),
                "typlen" => Inline(ColumnType::SmallInteger),
                "typbyval" => Inline(ColumnType::Boolean),
                "typtype" => Inline(ColumnType::InternalChar),
                "typcategory" => Inline(ColumnType::InternalChar),
                "typispreferred" => Inline(ColumnType::Boolean),
                "typisdefined" => Inline(ColumnType::Boolean),
                "typdelim" => Inline(ColumnType::InternalChar),
                "typrelid" => Inline(ColumnType::Oid),
                "typsubscript" => Inline(ColumnType::Regproc),
                "typelem" => Inline(ColumnType::Oid),
                "typarray" => Inline(ColumnType::Oid),
                "typinput" => Inline(ColumnType::Regproc),
                "typoutput" => Inline(ColumnType::Regproc),
                "typreceive" => Inline(ColumnType::Regproc),
                "typsend" => Inline(ColumnType::Regproc),
                "typmodin" => Inline(ColumnType::Regproc),
                "typmodout" => Inline(ColumnType::Regproc),
                "typanalyze" => Inline(ColumnType::Regproc),
                "typalign" => Inline(ColumnType::InternalChar),
                "typstorage" => Inline(ColumnType::InternalChar),
                "typnotnull" => Inline(ColumnType::Boolean),
                "typbasetype" => Inline(ColumnType::Oid),
                "typtypmod" => Inline(ColumnType::Integer),
                "typndims" => Inline(ColumnType::Integer),
                "typcollation" => Inline(ColumnType::Oid),
                "typdefaultbin" => Inline(ColumnType::PgNodeTree),
                "typdefault" => Inline(ColumnType::Text),
                "typacl" => Array(ColumnType::AclItem),
            ],
            Self::PgEnum => columns![
                "oid" => Inline(ColumnType::Oid),
                "enumtypid" => Inline(ColumnType::Oid),
                "enumsortorder" => Inline(ColumnType::Real),
                "enumlabel" => Inline(ColumnType::Name),
            ],
            Self::PgRange => columns![
                "rngtypid" => Inline(ColumnType::Oid),
                "rngsubtype" => Inline(ColumnType::Oid),
                "rngmultitypid" => Inline(ColumnType::Oid),
                "rngcollation" => Inline(ColumnType::Oid),
                "rngsubopc" => Inline(ColumnType::Oid),
                "rngcanonical" => Inline(ColumnType::Regproc),
                "rngsubdiff" => Inline(ColumnType::Regproc),
            ],
            Self::PgProc => columns![
                "oid" => Inline(ColumnType::Oid),
                "proname" => Inline(ColumnType::Name),
                "pronamespace" => Inline(ColumnType::Oid),
                "proowner" => Inline(ColumnType::Oid),
                "prolang" => Inline(ColumnType::Oid),
                "procost" => Inline(ColumnType::Real),
                "prorows" => Inline(ColumnType::Real),
                "provariadic" => Inline(ColumnType::Oid),
                "prosupport" => Inline(ColumnType::Regproc),
                "prokind" => Inline(ColumnType::InternalChar),
                "prosecdef" => Inline(ColumnType::Boolean),
                "proleakproof" => Inline(ColumnType::Boolean),
                "proisstrict" => Inline(ColumnType::Boolean),
                "proretset" => Inline(ColumnType::Boolean),
                "provolatile" => Inline(ColumnType::InternalChar),
                "proparallel" => Inline(ColumnType::InternalChar),
                "pronargs" => Inline(ColumnType::SmallInteger),
                "pronargdefaults" => Inline(ColumnType::SmallInteger),
                "prorettype" => Inline(ColumnType::Oid),
                "proargtypes" => Inline(ColumnType::OidVector),
                "proallargtypes" => Array(ColumnType::Oid),
                "proargmodes" => Array(ColumnType::InternalChar),
                "proargnames" => Array(ColumnType::Text),
                "proargdefaults" => Inline(ColumnType::PgNodeTree),
                "protrftypes" => Array(ColumnType::Oid),
                "prosrc" => Inline(ColumnType::Text),
                "probin" => Inline(ColumnType::Text),
                "prosqlbody" => Inline(ColumnType::PgNodeTree),
                "proconfig" => Array(ColumnType::Text),
                "proacl" => Array(ColumnType::AclItem),
            ],
            Self::PgLanguage => columns![
                "oid" => Inline(ColumnType::Oid),
                "lanname" => Inline(ColumnType::Name),
                "lanowner" => Inline(ColumnType::Oid),
                "lanispl" => Inline(ColumnType::Boolean),
                "lanpltrusted" => Inline(ColumnType::Boolean),
                "lanplcallfoid" => Inline(ColumnType::Oid),
                "laninline" => Inline(ColumnType::Oid),
                "lanvalidator" => Inline(ColumnType::Oid),
                "lanacl" => Array(ColumnType::AclItem),
            ],
            Self::PgExtension => columns![
                "oid" => Inline(ColumnType::Oid),
                "extname" => Inline(ColumnType::Name),
                "extowner" => Inline(ColumnType::Oid),
                "extnamespace" => Inline(ColumnType::Oid),
                "extrelocatable" => Inline(ColumnType::Boolean),
                "extversion" => Inline(ColumnType::Text),
                "extconfig" => Array(ColumnType::Oid),
                "extcondition" => Array(ColumnType::Text),
            ],
            Self::PgDatabase => columns![
                "oid" => Inline(ColumnType::Oid),
                "datname" => Inline(ColumnType::Name),
                "datdba" => Inline(ColumnType::Oid),
                "encoding" => Inline(ColumnType::Integer),
                "datlocprovider" => Inline(ColumnType::InternalChar),
                "datistemplate" => Inline(ColumnType::Boolean),
                "datallowconn" => Inline(ColumnType::Boolean),
                "dathasloginevt" => Inline(ColumnType::Boolean),
                "datconnlimit" => Inline(ColumnType::Integer),
                "datfrozenxid" => Inline(ColumnType::Xid),
                "datminmxid" => Inline(ColumnType::Xid),
                "dattablespace" => Inline(ColumnType::Oid),
                "datcollate" => Inline(ColumnType::Text),
                "datctype" => Inline(ColumnType::Text),
                "datlocale" => Inline(ColumnType::Text),
                "daticurules" => Inline(ColumnType::Text),
                "datcollversion" => Inline(ColumnType::Text),
                "datacl" => Array(ColumnType::AclItem),
            ],
            Self::PgAuthid => columns![
                "oid" => Inline(ColumnType::Oid),
                "rolname" => Inline(ColumnType::Name),
                "rolsuper" => Inline(ColumnType::Boolean),
                "rolinherit" => Inline(ColumnType::Boolean),
                "rolcreaterole" => Inline(ColumnType::Boolean),
                "rolcreatedb" => Inline(ColumnType::Boolean),
                "rolcanlogin" => Inline(ColumnType::Boolean),
                "rolreplication" => Inline(ColumnType::Boolean),
                "rolbypassrls" => Inline(ColumnType::Boolean),
                "rolconnlimit" => Inline(ColumnType::Integer),
                "rolpassword" => Inline(ColumnType::Text),
                "rolvaliduntil" => Inline(ColumnType::TimestampTz),
            ],
            Self::PgAuthMembers => columns![
                "oid" => Inline(ColumnType::Oid),
                "roleid" => Inline(ColumnType::Oid),
                "member" => Inline(ColumnType::Oid),
                "grantor" => Inline(ColumnType::Oid),
                "admin_option" => Inline(ColumnType::Boolean),
                "inherit_option" => Inline(ColumnType::Boolean),
                "set_option" => Inline(ColumnType::Boolean),
            ],
            Self::PgRoles => columns![
                "rolname" => Inline(ColumnType::Name),
                "rolsuper" => Inline(ColumnType::Boolean),
                "rolinherit" => Inline(ColumnType::Boolean),
                "rolcreaterole" => Inline(ColumnType::Boolean),
                "rolcreatedb" => Inline(ColumnType::Boolean),
                "rolcanlogin" => Inline(ColumnType::Boolean),
                "rolreplication" => Inline(ColumnType::Boolean),
                "rolconnlimit" => Inline(ColumnType::Integer),
                "rolpassword" => Inline(ColumnType::Text),
                "rolvaliduntil" => Inline(ColumnType::TimestampTz),
                "rolbypassrls" => Inline(ColumnType::Boolean),
                "rolconfig" => Array(ColumnType::Text),
                "oid" => Inline(ColumnType::Oid),
            ],
            Self::PgUser => columns![
                "usename" => Inline(ColumnType::Name),
                "usesysid" => Inline(ColumnType::Oid),
                "usecreatedb" => Inline(ColumnType::Boolean),
                "usesuper" => Inline(ColumnType::Boolean),
                "userepl" => Inline(ColumnType::Boolean),
                "usebypassrls" => Inline(ColumnType::Boolean),
                "passwd" => Inline(ColumnType::Text),
                "valuntil" => Inline(ColumnType::TimestampTz),
                "useconfig" => Array(ColumnType::Text),
            ],
            Self::PgCursors => columns![
                "name" => Inline(ColumnType::Text),
                "statement" => Inline(ColumnType::Text),
                "is_holdable" => Inline(ColumnType::Boolean),
                "is_binary" => Inline(ColumnType::Boolean),
                "is_scrollable" => Inline(ColumnType::Boolean),
                "creation_time" => Inline(ColumnType::TimestampTz),
            ],
            Self::PgPreparedStatements => columns![
                "name" => Inline(ColumnType::Text),
                "statement" => Inline(ColumnType::Text),
                "prepare_time" => Inline(ColumnType::TimestampTz),
                "parameter_types" => Array(ColumnType::Regtype),
                "result_types" => Array(ColumnType::Regtype),
                "from_sql" => Inline(ColumnType::Boolean),
                "generic_plans" => Inline(ColumnType::BigInteger),
                "custom_plans" => Inline(ColumnType::BigInteger),
            ],
            Self::PgSettings => columns![
                "name" => Inline(ColumnType::Text),
                "setting" => Inline(ColumnType::Text),
                "unit" => Inline(ColumnType::Text),
                "category" => Inline(ColumnType::Text),
                "short_desc" => Inline(ColumnType::Text),
                "extra_desc" => Inline(ColumnType::Text),
                "context" => Inline(ColumnType::Text),
                "vartype" => Inline(ColumnType::Text),
                "source" => Inline(ColumnType::Text),
                "min_val" => Inline(ColumnType::Text),
                "max_val" => Inline(ColumnType::Text),
                "enumvals" => Array(ColumnType::Text),
                "boot_val" => Inline(ColumnType::Text),
                "reset_val" => Inline(ColumnType::Text),
                "sourcefile" => Inline(ColumnType::Text),
                "sourceline" => Inline(ColumnType::Integer),
                "pending_restart" => Inline(ColumnType::Boolean),
            ],
            Self::PgDescription => columns![
                "objoid" => Inline(ColumnType::Oid),
                "classoid" => Inline(ColumnType::Oid),
                "objsubid" => Inline(ColumnType::Integer),
                "description" => Inline(ColumnType::Text),
            ],
            Self::PgDepend => columns![
                "classid" => Inline(ColumnType::Oid),
                "objid" => Inline(ColumnType::Oid),
                "objsubid" => Inline(ColumnType::Integer),
                "refclassid" => Inline(ColumnType::Oid),
                "refobjid" => Inline(ColumnType::Oid),
                "refobjsubid" => Inline(ColumnType::Integer),
                "deptype" => Inline(ColumnType::InternalChar),
            ],
            Self::PgShdepend => columns![
                "dbid" => Inline(ColumnType::Oid),
                "classid" => Inline(ColumnType::Oid),
                "objid" => Inline(ColumnType::Oid),
                "objsubid" => Inline(ColumnType::Integer),
                "refclassid" => Inline(ColumnType::Oid),
                "refobjid" => Inline(ColumnType::Oid),
                "deptype" => Inline(ColumnType::InternalChar),
            ],
            Self::PgMatviews => columns![
                "schemaname" => Inline(ColumnType::Name),
                "matviewname" => Inline(ColumnType::Name),
                "matviewowner" => Inline(ColumnType::Name),
                "tablespace" => Inline(ColumnType::Name),
                "hasindexes" => Inline(ColumnType::Boolean),
                "ispopulated" => Inline(ColumnType::Boolean),
                "definition" => Inline(ColumnType::Text),
            ],
            Self::PgSequences => columns![
                "schemaname" => Inline(ColumnType::Name),
                "sequencename" => Inline(ColumnType::Name),
                "sequenceowner" => Inline(ColumnType::Name),
                "data_type" => Inline(ColumnType::Regtype),
                "start_value" => Inline(ColumnType::BigInteger),
                "min_value" => Inline(ColumnType::BigInteger),
                "max_value" => Inline(ColumnType::BigInteger),
                "increment_by" => Inline(ColumnType::BigInteger),
                "cycle" => Inline(ColumnType::Boolean),
                "cache_size" => Inline(ColumnType::BigInteger),
                "last_value" => Inline(ColumnType::BigInteger),
            ],
            Self::AgGraph => columns![
                "graphid" => Inline(ColumnType::Oid),
                "name" => Inline(ColumnType::Name),
                "namespace" => Inline(ColumnType::Regnamespace),
            ],
            Self::AgLabel => columns![
                "name" => Inline(ColumnType::Name),
                "graph" => Inline(ColumnType::Oid),
                "id" => Domain(u32::try_from(oids::stable_oid("type", "ag_catalog.label_id")).expect("AGE type OID")),
                "kind" => Domain(u32::try_from(oids::stable_oid("type", "ag_catalog.label_kind")).expect("AGE type OID")),
                "relation" => Inline(ColumnType::Regclass),
                "seq_name" => Inline(ColumnType::Name),
            ],
        }
    }
}

fn ag_catalog_domain(name: &str) -> ColumnType {
    system_catalog_domain(ag_catalog_type_oid(name)).expect("AGE catalog type declaration")
}

/// Stable OID of an `ag_catalog` type; the extension assigns these from
/// the OID counter, so any collision-free assignment is faithful.
pub fn ag_catalog_type_oid(name: &str) -> u32 {
    let oid = oids::stable_oid("type", &format!("{AG_CATALOG_SCHEMA}.{name}"));
    u32::try_from(oid).unwrap_or(0)
}

/// AGE `label_id`: `int NOT NULL CHECK (VALUE > 0 AND VALUE <= 65535)`.
pub fn ag_label_id() -> ColumnType {
    ag_catalog_domain("label_id")
}

/// AGE `label_kind`: `"char" NOT NULL CHECK (VALUE = 'v' OR VALUE = 'e')`.
pub fn ag_label_kind() -> ColumnType {
    ag_catalog_domain("label_kind")
}

/// Internal type carrier for AGE's fixed-width `graphid` base type.
pub fn age_graphid() -> ColumnType {
    ag_catalog_domain("graphid")
}

/// Internal type carrier for AGE's JSONB-shaped `agtype` base type.
pub fn age_agtype() -> ColumnType {
    ag_catalog_domain("agtype")
}

pub fn ag_catalog_domains() -> Vec<ColumnType> {
    vec![ag_label_id(), ag_label_kind()]
}

/// The fixed system catalog domain with this OID: the `information_schema` domains and the AGE catalog types.
#[must_use]
pub fn system_catalog_domain(oid: u32) -> Option<ColumnType> {
    system_catalog_domain_with_control(oid, &uqa_core::memory::ProductionControl::uncontrolled())
        .expect("ordinary system domain projection cannot be cancelled or limited")
        .map(|ty| {
            ty.into_uncontrolled()
                .expect("ordinary system domain projection")
        })
}

fn cardinal_number() -> ColumnType {
    system_catalog_domain(13_307).expect("cardinal_number catalog declaration")
}

fn character_data() -> ColumnType {
    system_catalog_domain(13_310).expect("character_data catalog declaration")
}

fn sql_identifier() -> ColumnType {
    system_catalog_domain(13_312).expect("sql_identifier catalog declaration")
}

fn time_stamp() -> ColumnType {
    system_catalog_domain(13_318).expect("time_stamp catalog declaration")
}

fn yes_or_no() -> ColumnType {
    system_catalog_domain(13_320).expect("yes_or_no catalog declaration")
}

pub fn information_schema_domains() -> Vec<ColumnType> {
    vec![
        cardinal_number(),
        character_data(),
        sql_identifier(),
        time_stamp(),
        yes_or_no(),
    ]
}

fn information_routines_columns(visit: &mut dyn FnMut(&'static str, CatalogColumnType)) {
    macro_rules! columns {
        ($($name:literal => $ty:expr),* $(,)?) => {
            { $(visit($name, $ty);)* }
        };
    }

    columns![
        "specific_catalog" => Domain(13312),
        "specific_schema" => Domain(13312),
        "specific_name" => Domain(13312),
        "routine_catalog" => Domain(13312),
        "routine_schema" => Domain(13312),
        "routine_name" => Domain(13312),
        "routine_type" => Domain(13310),
        "module_catalog" => Domain(13312),
        "module_schema" => Domain(13312),
        "module_name" => Domain(13312),
        "udt_catalog" => Domain(13312),
        "udt_schema" => Domain(13312),
        "udt_name" => Domain(13312),
        "data_type" => Domain(13310),
        "character_maximum_length" => Domain(13307),
        "character_octet_length" => Domain(13307),
        "character_set_catalog" => Domain(13312),
        "character_set_schema" => Domain(13312),
        "character_set_name" => Domain(13312),
        "collation_catalog" => Domain(13312),
        "collation_schema" => Domain(13312),
        "collation_name" => Domain(13312),
        "numeric_precision" => Domain(13307),
        "numeric_precision_radix" => Domain(13307),
        "numeric_scale" => Domain(13307),
        "datetime_precision" => Domain(13307),
        "interval_type" => Domain(13310),
        "interval_precision" => Domain(13307),
        "type_udt_catalog" => Domain(13312),
        "type_udt_schema" => Domain(13312),
        "type_udt_name" => Domain(13312),
        "scope_catalog" => Domain(13312),
        "scope_schema" => Domain(13312),
        "scope_name" => Domain(13312),
        "maximum_cardinality" => Domain(13307),
        "dtd_identifier" => Domain(13312),
        "routine_body" => Domain(13310),
        "routine_definition" => Domain(13310),
        "external_name" => Domain(13310),
        "external_language" => Domain(13310),
        "parameter_style" => Domain(13310),
        "is_deterministic" => Domain(13320),
        "sql_data_access" => Domain(13310),
        "is_null_call" => Domain(13320),
        "sql_path" => Domain(13310),
        "schema_level_routine" => Domain(13320),
        "max_dynamic_result_sets" => Domain(13307),
        "is_user_defined_cast" => Domain(13320),
        "is_implicitly_invocable" => Domain(13320),
        "security_type" => Domain(13310),
        "to_sql_specific_catalog" => Domain(13312),
        "to_sql_specific_schema" => Domain(13312),
        "to_sql_specific_name" => Domain(13312),
        "as_locator" => Domain(13320),
        "created" => Domain(13318),
        "last_altered" => Domain(13318),
        "new_savepoint_level" => Domain(13320),
        "is_udt_dependent" => Domain(13320),
        "result_cast_from_data_type" => Domain(13310),
        "result_cast_as_locator" => Domain(13320),
        "result_cast_char_max_length" => Domain(13307),
        "result_cast_char_octet_length" => Domain(13307),
        "result_cast_char_set_catalog" => Domain(13312),
        "result_cast_char_set_schema" => Domain(13312),
        "result_cast_char_set_name" => Domain(13312),
        "result_cast_collation_catalog" => Domain(13312),
        "result_cast_collation_schema" => Domain(13312),
        "result_cast_collation_name" => Domain(13312),
        "result_cast_numeric_precision" => Domain(13307),
        "result_cast_numeric_precision_radix" => Domain(13307),
        "result_cast_numeric_scale" => Domain(13307),
        "result_cast_datetime_precision" => Domain(13307),
        "result_cast_interval_type" => Domain(13310),
        "result_cast_interval_precision" => Domain(13307),
        "result_cast_type_udt_catalog" => Domain(13312),
        "result_cast_type_udt_schema" => Domain(13312),
        "result_cast_type_udt_name" => Domain(13312),
        "result_cast_scope_catalog" => Domain(13312),
        "result_cast_scope_schema" => Domain(13312),
        "result_cast_scope_name" => Domain(13312),
        "result_cast_maximum_cardinality" => Domain(13307),
        "result_cast_dtd_identifier" => Domain(13312),
    ];
}

#[cfg(test)]
mod tests;

pub mod analysis;

pub mod array_type_names;

pub mod composite_type;
pub mod domain;
pub mod enum_type;
pub mod events;
pub mod foreign_server;
pub mod foreign_wrapper;
pub mod index;
pub mod range_type;
pub mod roles;
pub mod shell_type;

/// Namespaces the engine implements without a durable schema row: the
/// `PostgreSQL` system schemas and the Apache AGE catalog schema.
pub fn is_virtual_system_schema(name: &str) -> bool {
    matches!(name, "pg_catalog" | "information_schema" | "ag_catalog")
}

pub mod view;

pub mod session;

pub mod constraints;

pub mod dependencies;

pub const DATABASE_NAME: &str = "uqa";
pub const DATABASE_OID: i64 = 5;

pub mod stored_ast;

pub mod regrole_dependencies;

pub mod graph_oids;
pub mod relation_attributes;
pub mod relation_oids;
pub mod temporary_namespace;

pub mod security;

pub mod stored_view;

pub mod errors;

pub mod sequence_functions;

pub mod json_path;
