//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual `pg_catalog` relation builders.

mod attributes;
mod constraints;
mod indexes;
mod relations;
mod roles;
mod sequences;
mod types;
pub use attributes::{build_pg_attrdef, build_pg_attribute};
pub use constraints::build_pg_constraint;
pub use indexes::{
    build_pg_index, build_pg_indexes, catalog_index_relations, index_access_method_oid,
    CatalogIndexRelation,
};
pub use relations::{
    build_pg_database, build_pg_matviews, build_pg_tables, build_pg_views, pg_class_catalog_row,
    pg_class_row, pg_class_row_with_lifecycle, table_relation_oid_from, table_rowtype_oid_from,
};
pub use roles::{build_pg_auth_members, build_pg_roles, build_pg_user};
pub use sequences::build_pg_sequences;
pub use types::{build_pg_range, build_pg_type};
