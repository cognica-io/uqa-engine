//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated catalog, DDL, and mutation integration tests.

#[path = "engine_catalog/pg18_constraint_lifecycle.rs"]
mod pg18_constraint_lifecycle;
#[path = "sql_analyze_persistence.rs"]
mod sql_analyze_persistence;
#[path = "sql_analyzer_ddl.rs"]
mod sql_analyzer_ddl;
#[path = "sql_analyzer_lifecycle.rs"]
mod sql_analyzer_lifecycle;
#[path = "sql_check_fk_default.rs"]
mod sql_check_fk_default;
#[path = "sql_ctas.rs"]
mod sql_ctas;
#[path = "sql_ddl.rs"]
mod sql_ddl;
#[path = "sql_ddl_dependencies.rs"]
mod sql_ddl_dependencies;
#[path = "sql_ddl_extensions.rs"]
mod sql_ddl_extensions;
#[path = "sql_extended_ddl.rs"]
mod sql_extended_ddl;
#[path = "sql_foreign_ddl.rs"]
mod sql_foreign_ddl;
#[path = "sql_generated_columns.rs"]
mod sql_generated_columns;
#[path = "sql_index_catalog_lifecycle.rs"]
mod sql_index_catalog_lifecycle;
#[path = "sql_information_schema.rs"]
mod sql_information_schema;
#[path = "sql_merge.rs"]
mod sql_merge;
#[path = "sql_on_conflict.rs"]
mod sql_on_conflict;
#[path = "sql_point_update.rs"]
mod sql_point_update;
#[path = "sql_referential_actions.rs"]
mod sql_referential_actions;
#[path = "sql_rejected_ddl_lifecycle.rs"]
mod sql_rejected_ddl_lifecycle;
#[path = "engine_catalog/sql_relation_forms.rs"]
mod sql_relation_forms;
#[path = "sql_sequences.rs"]
mod sql_sequences;
#[path = "sql_set_search_path.rs"]
mod sql_set_search_path;
#[path = "sql_show_discard.rs"]
mod sql_show_discard;
#[path = "engine_catalog/sql_type_migration_temporal.rs"]
mod sql_type_migration_temporal;
#[path = "sql_types.rs"]
mod sql_types;
#[path = "sql_unique_constraint.rs"]
mod sql_unique_constraint;
#[path = "sql_update_delete.rs"]
mod sql_update_delete;
#[path = "sql_update_delete_coverage.rs"]
mod sql_update_delete_coverage;
#[path = "sql_update_from_delete_using.rs"]
mod sql_update_from_delete_using;
#[path = "sql_value_index.rs"]
mod sql_value_index;
#[path = "sql_views.rs"]
mod sql_views;
