//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 row types for the virtual system-catalog relations implemented by the engine.

use uqa_sql::ast::ColumnType;

use crate::Engine;

/// Namespace of the Apache AGE catalog relations, types, and functions.
pub(in crate::sql) const AG_CATALOG_SCHEMA: &str = "ag_catalog";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VirtualRelation {
    InformationSchemaCatalogName,
    InformationSchemata,
    InformationTables,
    InformationColumns,
    InformationViews,
    InformationRoutines,
    InformationSequences,
    InformationTableConstraints,
    InformationKeyColumnUsage,
    PgNamespace,
    PgClass,
    PgInherits,
    PgAttribute,
    PgAttrdef,
    PgConstraint,
    PgIndex,
    PgTables,
    PgViews,
    PgIndexes,
    PgType,
    PgProc,
    PgDatabase,
    PgRoles,
    PgUser,
    PgSettings,
    PgDescription,
    PgMatviews,
    PgSequences,
    AgGraph,
    AgLabel,
}

/// Resolve a relation reference to one of the engine's virtual catalog
/// relations. `information_schema` and `pg_catalog` names resolve
/// qualified or bare because `PostgreSQL` always searches `pg_catalog`;
/// the AGE relations resolve bare only while `ag_catalog` is on the
/// session `search_path`, exactly like the extension's schema.
pub(super) fn resolve_virtual_relation(engine: &Engine, name: &str) -> Option<VirtualRelation> {
    let lower = name.to_ascii_lowercase();
    if let Some(local) = lower.strip_prefix("ag_catalog.") {
        return resolve_ag_catalog_relation(local);
    }
    if !lower.contains('.') && engine.search_path_contains(AG_CATALOG_SCHEMA) {
        if let Some(relation) = resolve_ag_catalog_relation(&lower) {
            return Some(relation);
        }
    }
    let is_information_schema = lower.starts_with("information_schema.");
    let is_pg_catalog = lower.starts_with("pg_catalog.");
    let stripped = lower
        .strip_prefix("information_schema.")
        .or_else(|| lower.strip_prefix("pg_catalog."))
        .unwrap_or(&lower);
    match (is_information_schema, is_pg_catalog, stripped) {
        (true, _, "information_schema_catalog_name") => {
            Some(VirtualRelation::InformationSchemaCatalogName)
        }
        (true, _, "schemata") => Some(VirtualRelation::InformationSchemata),
        (true, _, "tables") => Some(VirtualRelation::InformationTables),
        (true, _, "columns") => Some(VirtualRelation::InformationColumns),
        (true, _, "views") => Some(VirtualRelation::InformationViews),
        (true, _, "routines") => Some(VirtualRelation::InformationRoutines),
        (true, _, "sequences") => Some(VirtualRelation::InformationSequences),
        (true, _, "table_constraints") => Some(VirtualRelation::InformationTableConstraints),
        (true, _, "key_column_usage") => Some(VirtualRelation::InformationKeyColumnUsage),
        (_, true, "pg_namespace") | (false, false, "pg_namespace") => {
            Some(VirtualRelation::PgNamespace)
        }
        (_, true, "pg_class") | (false, false, "pg_class") => Some(VirtualRelation::PgClass),
        (_, true, "pg_inherits") | (false, false, "pg_inherits") => {
            Some(VirtualRelation::PgInherits)
        }
        (_, true, "pg_attribute") | (false, false, "pg_attribute") => {
            Some(VirtualRelation::PgAttribute)
        }
        (_, true, "pg_attrdef") | (false, false, "pg_attrdef") => Some(VirtualRelation::PgAttrdef),
        (_, true, "pg_constraint") | (false, false, "pg_constraint") => {
            Some(VirtualRelation::PgConstraint)
        }
        (_, true, "pg_index") | (false, false, "pg_index") => Some(VirtualRelation::PgIndex),
        (_, true, "pg_tables") | (false, false, "pg_tables") => Some(VirtualRelation::PgTables),
        (_, true, "pg_views") | (false, false, "pg_views") => Some(VirtualRelation::PgViews),
        (_, true, "pg_indexes") | (false, false, "pg_indexes") => Some(VirtualRelation::PgIndexes),
        (_, true, "pg_type") | (false, false, "pg_type") => Some(VirtualRelation::PgType),
        (_, true, "pg_proc") | (false, false, "pg_proc") => Some(VirtualRelation::PgProc),
        (_, true, "pg_database") | (false, false, "pg_database") => {
            Some(VirtualRelation::PgDatabase)
        }
        (_, true, "pg_roles") | (false, false, "pg_roles") => Some(VirtualRelation::PgRoles),
        (_, true, "pg_user") | (false, false, "pg_user") => Some(VirtualRelation::PgUser),
        (_, true, "pg_settings") | (false, false, "pg_settings") => {
            Some(VirtualRelation::PgSettings)
        }
        (_, true, "pg_description") | (false, false, "pg_description") => {
            Some(VirtualRelation::PgDescription)
        }
        (_, true, "pg_matviews") | (false, false, "pg_matviews") => {
            Some(VirtualRelation::PgMatviews)
        }
        (_, true, "pg_sequences") | (false, false, "pg_sequences") => {
            Some(VirtualRelation::PgSequences)
        }
        _ => None,
    }
}

fn resolve_ag_catalog_relation(local: &str) -> Option<VirtualRelation> {
    match local {
        "ag_graph" => Some(VirtualRelation::AgGraph),
        "ag_label" => Some(VirtualRelation::AgLabel),
        _ => None,
    }
}

pub(in crate::sql) fn virtual_relation_schema(
    engine: &Engine,
    name: &str,
) -> Result<Option<Vec<(String, ColumnType)>>, uqa_sql::SQLError> {
    if let Some(relation) = resolve_virtual_relation(engine, name) {
        return Ok(Some(relation.schema()));
    }
    super::ag_catalog::age_label_relation_schema(engine, name)
}

pub(in crate::sql) fn virtual_relation_accepts_row_lock(
    engine: &Engine,
    name: &str,
) -> Option<bool> {
    resolve_virtual_relation(engine, name).map(VirtualRelation::accepts_row_lock)
}

impl VirtualRelation {
    const fn accepts_row_lock(self) -> bool {
        matches!(
            self,
            Self::PgNamespace
                | Self::PgClass
                | Self::PgInherits
                | Self::PgAttribute
                | Self::PgAttrdef
                | Self::PgConstraint
                | Self::PgIndex
                | Self::PgType
                | Self::PgProc
                | Self::PgDatabase
                | Self::PgDescription
                | Self::AgGraph
                | Self::AgLabel
        )
    }

    pub(super) fn schema(self) -> Vec<(String, ColumnType)> {
        macro_rules! columns {
            ($($name:literal => $ty:expr),* $(,)?) => {
                vec![$(($name.to_string(), $ty)),*]
            };
        }

        match self {
            Self::InformationSchemaCatalogName => columns![
                "catalog_name" => sql_identifier(),
            ],
            Self::InformationSchemata => columns![
                "catalog_name" => sql_identifier(),
                "schema_name" => sql_identifier(),
                "schema_owner" => sql_identifier(),
                "default_character_set_catalog" => sql_identifier(),
                "default_character_set_schema" => sql_identifier(),
                "default_character_set_name" => sql_identifier(),
                "sql_path" => character_data(),
            ],
            Self::InformationTables => columns![
                "table_catalog" => sql_identifier(),
                "table_schema" => sql_identifier(),
                "table_name" => sql_identifier(),
                "table_type" => character_data(),
                "self_referencing_column_name" => sql_identifier(),
                "reference_generation" => character_data(),
                "user_defined_type_catalog" => sql_identifier(),
                "user_defined_type_schema" => sql_identifier(),
                "user_defined_type_name" => sql_identifier(),
                "is_insertable_into" => yes_or_no(),
                "is_typed" => yes_or_no(),
                "commit_action" => character_data(),
            ],
            Self::InformationColumns => columns![
                "table_catalog" => sql_identifier(),
                "table_schema" => sql_identifier(),
                "table_name" => sql_identifier(),
                "column_name" => sql_identifier(),
                "ordinal_position" => cardinal_number(),
                "column_default" => character_data(),
                "is_nullable" => yes_or_no(),
                "data_type" => character_data(),
                "character_maximum_length" => cardinal_number(),
                "character_octet_length" => cardinal_number(),
                "numeric_precision" => cardinal_number(),
                "numeric_precision_radix" => cardinal_number(),
                "numeric_scale" => cardinal_number(),
                "datetime_precision" => cardinal_number(),
                "interval_type" => character_data(),
                "interval_precision" => cardinal_number(),
                "character_set_catalog" => sql_identifier(),
                "character_set_schema" => sql_identifier(),
                "character_set_name" => sql_identifier(),
                "collation_catalog" => sql_identifier(),
                "collation_schema" => sql_identifier(),
                "collation_name" => sql_identifier(),
                "domain_catalog" => sql_identifier(),
                "domain_schema" => sql_identifier(),
                "domain_name" => sql_identifier(),
                "udt_catalog" => sql_identifier(),
                "udt_schema" => sql_identifier(),
                "udt_name" => sql_identifier(),
                "scope_catalog" => sql_identifier(),
                "scope_schema" => sql_identifier(),
                "scope_name" => sql_identifier(),
                "maximum_cardinality" => cardinal_number(),
                "dtd_identifier" => sql_identifier(),
                "is_self_referencing" => yes_or_no(),
                "is_identity" => yes_or_no(),
                "identity_generation" => character_data(),
                "identity_start" => character_data(),
                "identity_increment" => character_data(),
                "identity_maximum" => character_data(),
                "identity_minimum" => character_data(),
                "identity_cycle" => yes_or_no(),
                "is_generated" => character_data(),
                "generation_expression" => character_data(),
                "is_updatable" => yes_or_no(),
            ],
            Self::InformationViews => columns![
                "table_catalog" => sql_identifier(),
                "table_schema" => sql_identifier(),
                "table_name" => sql_identifier(),
                "view_definition" => character_data(),
                "check_option" => character_data(),
                "is_updatable" => yes_or_no(),
                "is_insertable_into" => yes_or_no(),
                "is_trigger_updatable" => yes_or_no(),
                "is_trigger_deletable" => yes_or_no(),
                "is_trigger_insertable_into" => yes_or_no(),
            ],
            Self::InformationRoutines => information_routines_schema(),
            Self::InformationSequences => columns![
                "sequence_catalog" => sql_identifier(),
                "sequence_schema" => sql_identifier(),
                "sequence_name" => sql_identifier(),
                "data_type" => character_data(),
                "numeric_precision" => cardinal_number(),
                "numeric_precision_radix" => cardinal_number(),
                "numeric_scale" => cardinal_number(),
                "start_value" => character_data(),
                "minimum_value" => character_data(),
                "maximum_value" => character_data(),
                "increment" => character_data(),
                "cycle_option" => yes_or_no(),
            ],
            Self::InformationTableConstraints => columns![
                "constraint_catalog" => sql_identifier(),
                "constraint_schema" => sql_identifier(),
                "constraint_name" => sql_identifier(),
                "table_catalog" => sql_identifier(),
                "table_schema" => sql_identifier(),
                "table_name" => sql_identifier(),
                "constraint_type" => character_data(),
                "is_deferrable" => yes_or_no(),
                "initially_deferred" => yes_or_no(),
                "enforced" => yes_or_no(),
                "nulls_distinct" => yes_or_no(),
            ],
            Self::InformationKeyColumnUsage => columns![
                "constraint_catalog" => sql_identifier(),
                "constraint_schema" => sql_identifier(),
                "constraint_name" => sql_identifier(),
                "table_catalog" => sql_identifier(),
                "table_schema" => sql_identifier(),
                "table_name" => sql_identifier(),
                "column_name" => sql_identifier(),
                "ordinal_position" => cardinal_number(),
                "position_in_unique_constraint" => cardinal_number(),
            ],
            Self::PgNamespace => columns![
                "oid" => ColumnType::Oid,
                "nspname" => ColumnType::Name,
                "nspowner" => ColumnType::Oid,
                "nspacl" => array(ColumnType::AclItem),
            ],
            Self::PgClass => columns![
                "oid" => ColumnType::Oid,
                "relname" => ColumnType::Name,
                "relnamespace" => ColumnType::Oid,
                "reltype" => ColumnType::Oid,
                "reloftype" => ColumnType::Oid,
                "relowner" => ColumnType::Oid,
                "relam" => ColumnType::Oid,
                "relfilenode" => ColumnType::Oid,
                "reltablespace" => ColumnType::Oid,
                "relpages" => ColumnType::Integer,
                "reltuples" => ColumnType::Real,
                "relallvisible" => ColumnType::Integer,
                "relallfrozen" => ColumnType::Integer,
                "reltoastrelid" => ColumnType::Oid,
                "relhasindex" => ColumnType::Boolean,
                "relisshared" => ColumnType::Boolean,
                "relpersistence" => ColumnType::InternalChar,
                "relkind" => ColumnType::InternalChar,
                "relnatts" => ColumnType::SmallInteger,
                "relchecks" => ColumnType::SmallInteger,
                "relhasrules" => ColumnType::Boolean,
                "relhastriggers" => ColumnType::Boolean,
                "relhassubclass" => ColumnType::Boolean,
                "relrowsecurity" => ColumnType::Boolean,
                "relforcerowsecurity" => ColumnType::Boolean,
                "relispopulated" => ColumnType::Boolean,
                "relreplident" => ColumnType::InternalChar,
                "relispartition" => ColumnType::Boolean,
                "relrewrite" => ColumnType::Oid,
                "relfrozenxid" => ColumnType::Xid,
                "relminmxid" => ColumnType::Xid,
                "relacl" => array(ColumnType::AclItem),
                "reloptions" => array(ColumnType::Text),
                "relpartbound" => ColumnType::PgNodeTree,
            ],
            Self::PgInherits => columns![
                "inhrelid" => ColumnType::Oid,
                "inhparent" => ColumnType::Oid,
                "inhseqno" => ColumnType::Integer,
                "inhdetachpending" => ColumnType::Boolean,
            ],
            Self::PgAttribute => columns![
                "attrelid" => ColumnType::Oid,
                "attname" => ColumnType::Name,
                "atttypid" => ColumnType::Oid,
                "attlen" => ColumnType::SmallInteger,
                "attnum" => ColumnType::SmallInteger,
                "atttypmod" => ColumnType::Integer,
                "attndims" => ColumnType::SmallInteger,
                "attbyval" => ColumnType::Boolean,
                "attalign" => ColumnType::InternalChar,
                "attstorage" => ColumnType::InternalChar,
                "attcompression" => ColumnType::InternalChar,
                "attnotnull" => ColumnType::Boolean,
                "atthasdef" => ColumnType::Boolean,
                "atthasmissing" => ColumnType::Boolean,
                "attidentity" => ColumnType::InternalChar,
                "attgenerated" => ColumnType::InternalChar,
                "attisdropped" => ColumnType::Boolean,
                "attislocal" => ColumnType::Boolean,
                "attinhcount" => ColumnType::SmallInteger,
                "attcollation" => ColumnType::Oid,
                "attstattarget" => ColumnType::SmallInteger,
                "attacl" => array(ColumnType::AclItem),
                "attoptions" => array(ColumnType::Text),
                "attfdwoptions" => array(ColumnType::Text),
                "attmissingval" => ColumnType::AnyArray,
            ],
            Self::PgAttrdef => columns![
                "oid" => ColumnType::Oid,
                "adrelid" => ColumnType::Oid,
                "adnum" => ColumnType::SmallInteger,
                "adbin" => ColumnType::PgNodeTree,
            ],
            Self::PgConstraint => columns![
                "oid" => ColumnType::Oid,
                "conname" => ColumnType::Name,
                "connamespace" => ColumnType::Oid,
                "contype" => ColumnType::InternalChar,
                "condeferrable" => ColumnType::Boolean,
                "condeferred" => ColumnType::Boolean,
                "conenforced" => ColumnType::Boolean,
                "convalidated" => ColumnType::Boolean,
                "conrelid" => ColumnType::Oid,
                "contypid" => ColumnType::Oid,
                "conindid" => ColumnType::Oid,
                "conparentid" => ColumnType::Oid,
                "confrelid" => ColumnType::Oid,
                "confupdtype" => ColumnType::InternalChar,
                "confdeltype" => ColumnType::InternalChar,
                "confmatchtype" => ColumnType::InternalChar,
                "conislocal" => ColumnType::Boolean,
                "coninhcount" => ColumnType::SmallInteger,
                "connoinherit" => ColumnType::Boolean,
                "conperiod" => ColumnType::Boolean,
                "conkey" => array(ColumnType::SmallInteger),
                "confkey" => array(ColumnType::SmallInteger),
                "conpfeqop" => array(ColumnType::Oid),
                "conppeqop" => array(ColumnType::Oid),
                "conffeqop" => array(ColumnType::Oid),
                "confdelsetcols" => array(ColumnType::SmallInteger),
                "conexclop" => array(ColumnType::Oid),
                "conbin" => ColumnType::PgNodeTree,
            ],
            Self::PgIndex => columns![
                "indexrelid" => ColumnType::Oid,
                "indrelid" => ColumnType::Oid,
                "indnatts" => ColumnType::SmallInteger,
                "indnkeyatts" => ColumnType::SmallInteger,
                "indisunique" => ColumnType::Boolean,
                "indnullsnotdistinct" => ColumnType::Boolean,
                "indisprimary" => ColumnType::Boolean,
                "indisexclusion" => ColumnType::Boolean,
                "indimmediate" => ColumnType::Boolean,
                "indisclustered" => ColumnType::Boolean,
                "indisvalid" => ColumnType::Boolean,
                "indcheckxmin" => ColumnType::Boolean,
                "indisready" => ColumnType::Boolean,
                "indislive" => ColumnType::Boolean,
                "indisreplident" => ColumnType::Boolean,
                "indkey" => ColumnType::Int2Vector,
                "indcollation" => ColumnType::OidVector,
                "indclass" => ColumnType::OidVector,
                "indoption" => ColumnType::Int2Vector,
                "indexprs" => ColumnType::PgNodeTree,
                "indpred" => ColumnType::PgNodeTree,
            ],
            Self::PgTables => columns![
                "schemaname" => ColumnType::Name,
                "tablename" => ColumnType::Name,
                "tableowner" => ColumnType::Name,
                "tablespace" => ColumnType::Name,
                "hasindexes" => ColumnType::Boolean,
                "hasrules" => ColumnType::Boolean,
                "hastriggers" => ColumnType::Boolean,
                "rowsecurity" => ColumnType::Boolean,
            ],
            Self::PgViews => columns![
                "schemaname" => ColumnType::Name,
                "viewname" => ColumnType::Name,
                "viewowner" => ColumnType::Name,
                "definition" => ColumnType::Text,
            ],
            Self::PgIndexes => columns![
                "schemaname" => ColumnType::Name,
                "tablename" => ColumnType::Name,
                "indexname" => ColumnType::Name,
                "tablespace" => ColumnType::Name,
                "indexdef" => ColumnType::Text,
            ],
            Self::PgType => columns![
                "oid" => ColumnType::Oid,
                "typname" => ColumnType::Name,
                "typnamespace" => ColumnType::Oid,
                "typowner" => ColumnType::Oid,
                "typlen" => ColumnType::SmallInteger,
                "typbyval" => ColumnType::Boolean,
                "typtype" => ColumnType::InternalChar,
                "typcategory" => ColumnType::InternalChar,
                "typispreferred" => ColumnType::Boolean,
                "typisdefined" => ColumnType::Boolean,
                "typdelim" => ColumnType::InternalChar,
                "typrelid" => ColumnType::Oid,
                "typsubscript" => ColumnType::Regproc,
                "typelem" => ColumnType::Oid,
                "typarray" => ColumnType::Oid,
                "typinput" => ColumnType::Regproc,
                "typoutput" => ColumnType::Regproc,
                "typreceive" => ColumnType::Regproc,
                "typsend" => ColumnType::Regproc,
                "typmodin" => ColumnType::Regproc,
                "typmodout" => ColumnType::Regproc,
                "typanalyze" => ColumnType::Regproc,
                "typalign" => ColumnType::InternalChar,
                "typstorage" => ColumnType::InternalChar,
                "typnotnull" => ColumnType::Boolean,
                "typbasetype" => ColumnType::Oid,
                "typtypmod" => ColumnType::Integer,
                "typndims" => ColumnType::Integer,
                "typcollation" => ColumnType::Oid,
                "typdefaultbin" => ColumnType::PgNodeTree,
                "typdefault" => ColumnType::Text,
                "typacl" => array(ColumnType::AclItem),
            ],
            Self::PgProc => columns![
                "oid" => ColumnType::Oid,
                "proname" => ColumnType::Name,
                "pronamespace" => ColumnType::Oid,
                "proowner" => ColumnType::Oid,
                "prolang" => ColumnType::Oid,
                "procost" => ColumnType::Real,
                "prorows" => ColumnType::Real,
                "provariadic" => ColumnType::Oid,
                "prosupport" => ColumnType::Regproc,
                "prokind" => ColumnType::InternalChar,
                "prosecdef" => ColumnType::Boolean,
                "proleakproof" => ColumnType::Boolean,
                "proisstrict" => ColumnType::Boolean,
                "proretset" => ColumnType::Boolean,
                "provolatile" => ColumnType::InternalChar,
                "proparallel" => ColumnType::InternalChar,
                "pronargs" => ColumnType::SmallInteger,
                "pronargdefaults" => ColumnType::SmallInteger,
                "prorettype" => ColumnType::Oid,
                "proargtypes" => ColumnType::OidVector,
                "proallargtypes" => array(ColumnType::Oid),
                "proargmodes" => array(ColumnType::InternalChar),
                "proargnames" => array(ColumnType::Text),
                "proargdefaults" => ColumnType::PgNodeTree,
                "protrftypes" => array(ColumnType::Oid),
                "prosrc" => ColumnType::Text,
                "probin" => ColumnType::Text,
                "prosqlbody" => ColumnType::PgNodeTree,
                "proconfig" => array(ColumnType::Text),
                "proacl" => array(ColumnType::AclItem),
            ],
            Self::PgDatabase => columns![
                "oid" => ColumnType::Oid,
                "datname" => ColumnType::Name,
                "datdba" => ColumnType::Oid,
                "encoding" => ColumnType::Integer,
                "datlocprovider" => ColumnType::InternalChar,
                "datistemplate" => ColumnType::Boolean,
                "datallowconn" => ColumnType::Boolean,
                "dathasloginevt" => ColumnType::Boolean,
                "datconnlimit" => ColumnType::Integer,
                "datfrozenxid" => ColumnType::Xid,
                "datminmxid" => ColumnType::Xid,
                "dattablespace" => ColumnType::Oid,
                "datcollate" => ColumnType::Text,
                "datctype" => ColumnType::Text,
                "datlocale" => ColumnType::Text,
                "daticurules" => ColumnType::Text,
                "datcollversion" => ColumnType::Text,
                "datacl" => array(ColumnType::AclItem),
            ],
            Self::PgRoles => columns![
                "rolname" => ColumnType::Name,
                "rolsuper" => ColumnType::Boolean,
                "rolinherit" => ColumnType::Boolean,
                "rolcreaterole" => ColumnType::Boolean,
                "rolcreatedb" => ColumnType::Boolean,
                "rolcanlogin" => ColumnType::Boolean,
                "rolreplication" => ColumnType::Boolean,
                "rolconnlimit" => ColumnType::Integer,
                "rolpassword" => ColumnType::Text,
                "rolvaliduntil" => ColumnType::TimestampTz,
                "rolbypassrls" => ColumnType::Boolean,
                "rolconfig" => array(ColumnType::Text),
                "oid" => ColumnType::Oid,
            ],
            Self::PgUser => columns![
                "usename" => ColumnType::Name,
                "usesysid" => ColumnType::Oid,
                "usecreatedb" => ColumnType::Boolean,
                "usesuper" => ColumnType::Boolean,
                "userepl" => ColumnType::Boolean,
                "usebypassrls" => ColumnType::Boolean,
                "passwd" => ColumnType::Text,
                "valuntil" => ColumnType::TimestampTz,
                "useconfig" => array(ColumnType::Text),
            ],
            Self::PgSettings => columns![
                "name" => ColumnType::Text,
                "setting" => ColumnType::Text,
                "unit" => ColumnType::Text,
                "category" => ColumnType::Text,
                "short_desc" => ColumnType::Text,
                "extra_desc" => ColumnType::Text,
                "context" => ColumnType::Text,
                "vartype" => ColumnType::Text,
                "source" => ColumnType::Text,
                "min_val" => ColumnType::Text,
                "max_val" => ColumnType::Text,
                "enumvals" => array(ColumnType::Text),
                "boot_val" => ColumnType::Text,
                "reset_val" => ColumnType::Text,
                "sourcefile" => ColumnType::Text,
                "sourceline" => ColumnType::Integer,
                "pending_restart" => ColumnType::Boolean,
            ],
            Self::PgDescription => columns![
                "objoid" => ColumnType::Oid,
                "classoid" => ColumnType::Oid,
                "objsubid" => ColumnType::Integer,
                "description" => ColumnType::Text,
            ],
            Self::PgMatviews => columns![
                "schemaname" => ColumnType::Name,
                "matviewname" => ColumnType::Name,
                "matviewowner" => ColumnType::Name,
                "tablespace" => ColumnType::Name,
                "hasindexes" => ColumnType::Boolean,
                "ispopulated" => ColumnType::Boolean,
                "definition" => ColumnType::Text,
            ],
            Self::PgSequences => columns![
                "schemaname" => ColumnType::Name,
                "sequencename" => ColumnType::Name,
                "sequenceowner" => ColumnType::Name,
                "data_type" => ColumnType::Regtype,
                "start_value" => ColumnType::BigInteger,
                "min_value" => ColumnType::BigInteger,
                "max_value" => ColumnType::BigInteger,
                "increment_by" => ColumnType::BigInteger,
                "cycle" => ColumnType::Boolean,
                "cache_size" => ColumnType::BigInteger,
                "last_value" => ColumnType::BigInteger,
            ],
            Self::AgGraph => columns![
                "graphid" => ColumnType::Oid,
                "name" => ColumnType::Name,
                "namespace" => ColumnType::Regnamespace,
            ],
            Self::AgLabel => columns![
                "name" => ColumnType::Name,
                "graph" => ColumnType::Oid,
                "id" => ag_label_id(),
                "kind" => ag_label_kind(),
                "relation" => ColumnType::Regclass,
                "seq_name" => ColumnType::Name,
            ],
        }
    }
}

fn ag_catalog_domain(name: &str, base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: AG_CATALOG_SCHEMA.into(),
        name: name.into(),
        oid: ag_catalog_type_oid(name),
        base: Box::new(base),
    }
}

/// Stable OID of an `ag_catalog` type; the extension assigns these from
/// the OID counter, so any collision-free assignment is faithful.
pub(super) fn ag_catalog_type_oid(name: &str) -> u32 {
    let oid = super::helpers::stable_oid("type", &format!("{AG_CATALOG_SCHEMA}.{name}"));
    u32::try_from(oid).unwrap_or(0)
}

/// AGE `label_id`: `int NOT NULL CHECK (VALUE > 0 AND VALUE <= 65535)`.
pub(super) fn ag_label_id() -> ColumnType {
    ag_catalog_domain("label_id", ColumnType::Integer)
}

/// AGE `label_kind`: `"char" NOT NULL CHECK (VALUE = 'v' OR VALUE = 'e')`.
pub(super) fn ag_label_kind() -> ColumnType {
    ag_catalog_domain("label_kind", ColumnType::InternalChar)
}

/// Internal type carrier for AGE's fixed-width `graphid` base type.
pub(super) fn age_graphid() -> ColumnType {
    ag_catalog_domain("graphid", ColumnType::BigInteger)
}

/// Internal type carrier for AGE's JSONB-shaped `agtype` base type.
pub(super) fn age_agtype() -> ColumnType {
    ag_catalog_domain("agtype", ColumnType::JsonB)
}

pub(super) fn ag_catalog_domains() -> Vec<ColumnType> {
    vec![ag_label_id(), ag_label_kind()]
}

fn array(element: ColumnType) -> ColumnType {
    ColumnType::Array(Box::new(element))
}

fn information_schema_domain(name: &str, oid: u32, base: ColumnType) -> ColumnType {
    ColumnType::Domain {
        schema: "information_schema".into(),
        name: name.into(),
        oid,
        base: Box::new(base),
    }
}

fn cardinal_number() -> ColumnType {
    information_schema_domain("cardinal_number", 13_307, ColumnType::Integer)
}

fn character_data() -> ColumnType {
    information_schema_domain("character_data", 13_310, ColumnType::Varchar(None))
}

fn sql_identifier() -> ColumnType {
    information_schema_domain("sql_identifier", 13_312, ColumnType::Name)
}

fn time_stamp() -> ColumnType {
    information_schema_domain("time_stamp", 13_318, ColumnType::TimestampTz)
}

fn yes_or_no() -> ColumnType {
    information_schema_domain("yes_or_no", 13_320, ColumnType::Varchar(Some(3)))
}

pub(super) fn information_schema_domains() -> Vec<ColumnType> {
    vec![
        cardinal_number(),
        character_data(),
        sql_identifier(),
        time_stamp(),
        yes_or_no(),
    ]
}

fn information_routines_schema() -> Vec<(String, ColumnType)> {
    macro_rules! columns {
        ($($name:literal => $ty:expr),* $(,)?) => {
            vec![$(($name.to_string(), $ty)),*]
        };
    }

    columns![
        "specific_catalog" => sql_identifier(),
        "specific_schema" => sql_identifier(),
        "specific_name" => sql_identifier(),
        "routine_catalog" => sql_identifier(),
        "routine_schema" => sql_identifier(),
        "routine_name" => sql_identifier(),
        "routine_type" => character_data(),
        "module_catalog" => sql_identifier(),
        "module_schema" => sql_identifier(),
        "module_name" => sql_identifier(),
        "udt_catalog" => sql_identifier(),
        "udt_schema" => sql_identifier(),
        "udt_name" => sql_identifier(),
        "data_type" => character_data(),
        "character_maximum_length" => cardinal_number(),
        "character_octet_length" => cardinal_number(),
        "character_set_catalog" => sql_identifier(),
        "character_set_schema" => sql_identifier(),
        "character_set_name" => sql_identifier(),
        "collation_catalog" => sql_identifier(),
        "collation_schema" => sql_identifier(),
        "collation_name" => sql_identifier(),
        "numeric_precision" => cardinal_number(),
        "numeric_precision_radix" => cardinal_number(),
        "numeric_scale" => cardinal_number(),
        "datetime_precision" => cardinal_number(),
        "interval_type" => character_data(),
        "interval_precision" => cardinal_number(),
        "type_udt_catalog" => sql_identifier(),
        "type_udt_schema" => sql_identifier(),
        "type_udt_name" => sql_identifier(),
        "scope_catalog" => sql_identifier(),
        "scope_schema" => sql_identifier(),
        "scope_name" => sql_identifier(),
        "maximum_cardinality" => cardinal_number(),
        "dtd_identifier" => sql_identifier(),
        "routine_body" => character_data(),
        "routine_definition" => character_data(),
        "external_name" => character_data(),
        "external_language" => character_data(),
        "parameter_style" => character_data(),
        "is_deterministic" => yes_or_no(),
        "sql_data_access" => character_data(),
        "is_null_call" => yes_or_no(),
        "sql_path" => character_data(),
        "schema_level_routine" => yes_or_no(),
        "max_dynamic_result_sets" => cardinal_number(),
        "is_user_defined_cast" => yes_or_no(),
        "is_implicitly_invocable" => yes_or_no(),
        "security_type" => character_data(),
        "to_sql_specific_catalog" => sql_identifier(),
        "to_sql_specific_schema" => sql_identifier(),
        "to_sql_specific_name" => sql_identifier(),
        "as_locator" => yes_or_no(),
        "created" => time_stamp(),
        "last_altered" => time_stamp(),
        "new_savepoint_level" => yes_or_no(),
        "is_udt_dependent" => yes_or_no(),
        "result_cast_from_data_type" => character_data(),
        "result_cast_as_locator" => yes_or_no(),
        "result_cast_char_max_length" => cardinal_number(),
        "result_cast_char_octet_length" => cardinal_number(),
        "result_cast_char_set_catalog" => sql_identifier(),
        "result_cast_char_set_schema" => sql_identifier(),
        "result_cast_char_set_name" => sql_identifier(),
        "result_cast_collation_catalog" => sql_identifier(),
        "result_cast_collation_schema" => sql_identifier(),
        "result_cast_collation_name" => sql_identifier(),
        "result_cast_numeric_precision" => cardinal_number(),
        "result_cast_numeric_precision_radix" => cardinal_number(),
        "result_cast_numeric_scale" => cardinal_number(),
        "result_cast_datetime_precision" => cardinal_number(),
        "result_cast_interval_type" => character_data(),
        "result_cast_interval_precision" => cardinal_number(),
        "result_cast_type_udt_catalog" => sql_identifier(),
        "result_cast_type_udt_schema" => sql_identifier(),
        "result_cast_type_udt_name" => sql_identifier(),
        "result_cast_scope_catalog" => sql_identifier(),
        "result_cast_scope_schema" => sql_identifier(),
        "result_cast_scope_name" => sql_identifier(),
        "result_cast_maximum_cardinality" => cardinal_number(),
        "result_cast_dtd_identifier" => sql_identifier(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg18_catalog_shapes_include_empty_relations_and_removed_columns() {
        let engine = Engine::new();
        let description = virtual_relation_schema(&engine, "pg_catalog.pg_description")
            .unwrap()
            .unwrap();
        assert_eq!(description.len(), 4);
        assert_eq!(description[0], ("objoid".into(), ColumnType::Oid));

        let attrdef = virtual_relation_schema(&engine, "pg_attrdef")
            .unwrap()
            .unwrap();
        assert_eq!(
            attrdef
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["oid", "adrelid", "adnum", "adbin"]
        );
    }

    #[test]
    fn pg18_information_schema_domains_retain_their_oid_identity() {
        let engine = Engine::new();
        let routines = virtual_relation_schema(&engine, "information_schema.routines")
            .unwrap()
            .unwrap();
        assert_eq!(routines.len(), 82);
        assert!(matches!(
            routines[0].1,
            ColumnType::Domain { oid: 13_312, .. }
        ));
        assert!(matches!(
            routines[54].1,
            ColumnType::Domain { oid: 13_318, .. }
        ));
    }

    #[test]
    fn ag_catalog_relations_resolve_qualified_or_through_the_search_path() {
        let engine = Engine::new();
        assert_eq!(
            resolve_virtual_relation(&engine, "ag_catalog.ag_graph"),
            Some(VirtualRelation::AgGraph)
        );
        assert_eq!(
            resolve_virtual_relation(&engine, "AG_CATALOG.AG_LABEL"),
            Some(VirtualRelation::AgLabel)
        );
        assert_eq!(resolve_virtual_relation(&engine, "ag_graph"), None);
        engine
            .set_variable("search_path", "ag_catalog, \"$user\", public")
            .unwrap();
        assert_eq!(
            resolve_virtual_relation(&engine, "ag_graph"),
            Some(VirtualRelation::AgGraph)
        );
        assert_eq!(
            resolve_virtual_relation(&engine, "ag_label"),
            Some(VirtualRelation::AgLabel)
        );
        assert_eq!(resolve_virtual_relation(&engine, "public.ag_graph"), None);

        let graph = virtual_relation_schema(&engine, "ag_catalog.ag_graph")
            .unwrap()
            .unwrap();
        assert_eq!(
            graph
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["graphid", "name", "namespace"]
        );
        assert_eq!(graph[2].1, ColumnType::Regnamespace);
        let label = virtual_relation_schema(&engine, "ag_catalog.ag_label")
            .unwrap()
            .unwrap();
        assert_eq!(
            label
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["name", "graph", "id", "kind", "relation", "seq_name"]
        );
        assert!(matches!(&label[2].1, ColumnType::Domain { name, .. } if name == "label_id"));
        assert!(matches!(&label[3].1, ColumnType::Domain { name, .. } if name == "label_kind"));
    }
}
