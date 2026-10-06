//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog` relation builders.

mod attributes;
mod composites;
mod constraint_definitions;
mod constraints;
mod indexes;
mod languages;
mod relations;
mod roles;
mod row_types;
mod sequences;
mod types;
pub use attributes::{attrdef_catalog_oid, build_pg_attrdef, build_pg_attribute};
pub use composites::composite_class_rows;
pub use constraint_definitions::pg_get_constraintdef_value;
pub use constraints::build_pg_constraint;
pub(crate) use constraints::{constraint_index_oid, constraint_parent_oid, constraint_row_oid};
pub(crate) use indexes::legacy::catalog_index_relations as legacy_index_relations;
pub use indexes::{
    build_pg_index, build_pg_indexes, catalog_index_relations, index_access_method_oid,
    CatalogIndexRelation,
};
pub use languages::build_pg_language;
pub(super) use languages::language_class_row;
pub use relations::{
    build_pg_database, build_pg_matviews, build_pg_tables, build_pg_views, pg_class_catalog_row,
    pg_class_row, pg_class_row_with_lifecycle, relation_identity_for_oid, table_relation_oid_from,
    table_rowtype_oid_from,
};
pub use roles::{build_pg_auth_members, build_pg_authid, build_pg_roles, build_pg_user};
pub use sequences::build_pg_sequences;
pub use types::{build_pg_enum, build_pg_range, build_pg_type, build_pg_type_without_defaults};
