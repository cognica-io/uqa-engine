//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 automatically updatable view coverage.

use super::*;

#[path = "automatic_updatability/classification.rs"]
mod classification;
#[path = "automatic_updatability/correlation.rs"]
mod correlation;
#[path = "automatic_updatability/insert.rs"]
mod insert;
#[path = "automatic_updatability/merge.rs"]
mod merge;
#[path = "automatic_updatability/returning.rs"]
mod returning;
#[path = "automatic_updatability/rules.rs"]
mod rules;
#[path = "automatic_updatability/update_delete.rs"]
mod update_delete;

fn automatic_view_engine() -> Engine {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE automatic_base (
            id INTEGER PRIMARY KEY,
            value TEXT NOT NULL,
            visible BOOLEAN NOT NULL DEFAULT true,
            quantity INTEGER NOT NULL DEFAULT 7
        )",
    );
    exec(
        &engine,
        "CREATE VIEW automatic_items (item_id, label, visible, doubled) AS
         SELECT id, value, visible, quantity * 2 FROM automatic_base WHERE visible",
    );
    engine
}

#[test]
fn automatically_updatable_views_match_postgresql_18() {
    insert::assert_simple_view_insert_defaults_upsert_and_computed_columns();
    returning::assert_update_from_delete_using_returning_and_visibility();
    returning::assert_view_row_type_is_the_dml_name_boundary();
    returning::assert_source_old_and_new_aliases_remain_source_relations();
    rules::assert_view_rules_precede_automatic_rewrite();
    rules::assert_nested_view_rules_run_at_each_rewrite_layer();
    insert::assert_nested_instead_rules_stop_lower_rewrite_layers();
    rules::assert_rule_and_trigger_rewrite_order();
    rules::assert_update_delete_rule_and_trigger_rewrite_order();
    insert::assert_nested_insert_rule_suppression_and_order();
    insert::assert_suppressed_rule_insert_does_not_prepare_base_row();
    returning::assert_nested_rule_returning_provider_and_lazy_projection();
    returning::assert_rule_projection_defaults_returning_and_command_tags();
    rules::assert_rule_suppression_defers_expressions_and_statement_triggers();
    insert::assert_rule_insert_images_conflicts_and_lazy_sources();
    rules::assert_rule_action_cardinality_matches_postgresql_18();
    insert::assert_suppressed_nested_and_direct_view_dml_is_lazy();
    insert::assert_rule_condition_case_projection_is_lazy();
    correlation::assert_automatic_view_subqueries_keep_the_complete_dml_scope();
    classification::assert_conditional_instead_rules_do_not_make_views_updatable();
    classification::assert_nested_conditional_instead_rule_rejects_statement();
    rules::assert_view_rule_actions_with_duplicate_user_ids_survive_reopen();
    correlation::assert_correlated_view_references_use_the_public_row_type();
    correlation::assert_unaliased_derived_source_keeps_source_only_names();
    correlation::assert_correlated_source_only_names_remain_source_bound();
    update_delete::assert_base_triggers_replace_view_statement_triggers();
    update_delete::assert_local_and_cascaded_check_options();
    insert::assert_nested_views_preserve_aliases_and_defaults();
    classification::assert_view_star_row_type_is_fixed_at_creation();
    insert::assert_partition_tableoid_uses_the_physical_relation();
    merge::assert_automatic_view_merge_rewrites_all_actions();
    merge::assert_automatic_view_merge_filters_target_rows();
    merge::assert_automatic_view_merge_check_options();
    merge::assert_automatic_view_merge_errors();
    classification::assert_non_updatable_views_and_catalog_flags();
    classification::assert_rule_updatability_catalog_flags();
    classification::assert_check_option_error_order_and_mapped_duplicates();
    classification::assert_check_option_definition_over_non_updatable_source();
    insert::assert_only_partition_view_insert_routes_to_a_partition();
    insert::assert_nested_rule_backed_computed_columns();
    insert::assert_nested_nonautomatic_rule_backed_view_executes();
    insert::assert_nonautomatic_rule_boundary_preserves_outer_layers();
    insert::assert_rule_backed_view_inputs_are_evaluated_lazily();
    classification::assert_rule_catalog_flags_ignore_replication_mode();
    classification::assert_select_star_without_a_relation_is_rejected();
    correlation::assert_unqualified_system_columns_use_target_qualification_cardinality();
}
