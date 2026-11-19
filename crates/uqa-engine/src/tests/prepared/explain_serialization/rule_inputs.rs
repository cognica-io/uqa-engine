//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each independent `PostgreSQL` rewrite-rule reference owns a bounded provider test.

use crate::tests::providers::Provider;
use uqa_core::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Reference {
    Insert,
    UpdateDelete,
}

impl Reference {
    fn read(self) -> serde_json::Value {
        let text = match self {
            Self::Insert => {
                include_str!("../../../../../../tests/parity/pg18/rules_query_inputs.expected.json")
            }
            Self::UpdateDelete => include_str!(
                "../../../../../../tests/parity/pg18/rules_update_delete_inputs.expected.json"
            ),
        };
        serde_json::from_str(text).unwrap()
    }
}

macro_rules! reference_cases {
    ($($identifier:ident: $reference:ident => $name:literal),+ $(,)?) => {
        #[rstest::rstest]
        $(#[case::$identifier(Reference::$reference, $name)])+
        fn rule_query_inputs_match_postgresql(
            #[case] reference: Reference,
            #[case] name: &str,
            #[values(Provider::Memory, Provider::SQLiteNative, Provider::SQLiteKeyValue, Provider::Redb)] provider: Provider,
        ) {
            verify_case(provider, reference, name);
        }

        #[test]
        fn rule_query_inputs_cover_every_reference_case() {
            use std::collections::BTreeSet;
            let declared = [$( (Reference::$reference, $name.to_owned()) ),+];
            let selected: BTreeSet<_> = declared.iter().cloned().collect();
            assert_eq!(selected.len(), declared.len(), "duplicate reference registration");
            let mut expected = BTreeSet::new();
            for reference in [Reference::Insert, Reference::UpdateDelete] {
                let document = reference.read();
                for case in document["cases"].as_array().unwrap() {
                    assert!(expected.insert((reference, case["name"].as_str().unwrap().to_owned())), "duplicate reference identity");
                }
            }
            assert_eq!(selected, expected, "every captured PostgreSQL case must execute");
        }
    };
}

reference_cases! {
    action_values: Insert => "action-values",
    action_multiple_values: Insert => "action-multiple-values",
    action_select: Insert => "action-select",
    independent_actions: Insert => "independent-actions",
    unused_volatile_output: Insert => "unused-volatile-output",
    unused_error_output: Insert => "unused-error-output",
    condition: Insert => "condition",
    whole_row: Insert => "whole-row",
    used_default: Insert => "used-default",
    unused_default: Insert => "unused-default",
    query_cte: Insert => "query-cte",
    query_cte_multiple_actions: Insert => "query-cte-multiple-actions",
    nested_rules: Insert => "nested-rules",
    empty_query: Insert => "empty-query",
    nothing: Insert => "nothing",
    prepared_input: Insert => "prepared-input",
    cte_returning: Insert => "cte-returning",
    update_action: Insert => "update-action",
    delete_action: Insert => "delete-action",
    automatic_view: Insert => "automatic-view",
    also_rule: Insert => "also-rule",
    conditional_instead: Insert => "conditional-instead",
    view_rule_layers: Insert => "view-rule-layers",
    partial_assignment: Insert => "partial-assignment",
    partial_array_volatile_bounds: Insert => "partial-array-volatile-bounds",
    partial_composite_fields: Insert => "partial-composite-fields",
    partial_nested_volatile_bounds: Insert => "partial-nested-volatile-bounds",
    implicit_used_default: Insert => "implicit-used-default",
    implicit_unused_default: Insert => "implicit-unused-default",
    implicit_view_default: Insert => "implicit-view-default",
    null_slice_bound_rule: Insert => "null-slice-bound-rule",
    null_slice_bound_ordinary: Insert => "null-slice-bound-ordinary",
    cte_automatic_view_multiple_actions: Insert => "cte-automatic-view-multiple-actions",
    cte_view_original_suppressed: Insert => "cte-view-original-suppressed",
    cte_also_rule: Insert => "cte-also-rule",
    unused_scalar_error: Insert => "unused-scalar-error",
    unused_nested_scalar_error: Insert => "unused-nested-scalar-error",
    unused_values_scalar_error: Insert => "unused-values-scalar-error",
    used_volatile_unused_scalar_error: Insert => "used-volatile-unused-scalar-error",
    unused_scalar_volatile: Insert => "unused-scalar-volatile",
    unused_values_scalar_volatile: Insert => "unused-values-scalar-volatile",
    unused_cte_shadowing_view: Insert => "unused-cte-shadowing-view",
    unused_nested_cte_shadowing_view: Insert => "unused-nested-cte-shadowing-view",
    unused_outer_cte_shadowing_view: Insert => "unused-outer-cte-shadowing-view",
    unused_query_cte_shadowing_view: Insert => "unused-query-cte-shadowing-view",
    unused_outer_nested_cte_shadowing_view: Insert => "unused-outer-nested-cte-shadowing-view",
    unused_derived_cte_shadowing_view: Insert => "unused-derived-cte-shadowing-view",
    unused_outer_volatile_cte: Insert => "unused-outer-volatile-cte",
    unused_inner_volatile_cte: Insert => "unused-inner-volatile-cte",
    unused_cte_catalog_view_scope: Insert => "unused-cte-catalog-view-scope",
    unused_recursive_forward_cte: Insert => "unused-recursive-forward-cte",
    unused_quoted_cte_shadowing_view: Insert => "unused-quoted-cte-shadowing-view",
    table_query_constant_action: Insert => "table-query-constant-action",
    table_query_default_action: Insert => "table-query-default-action",
    empty_table_default_action: Insert => "empty-table-default-action",
    caller_only: Insert => "caller-only",
    owner_only: Insert => "owner-only",
    bound_caller_only: Insert => "bound-caller-only",
    condition_caller_only: Insert => "condition-caller-only",
    bound_owner_only: Insert => "bound-owner-only",
    condition_owner_only: Insert => "condition-owner-only",
    values_single: Insert => "values-single",
    values_multiple: Insert => "values-multiple",
    values_single_unused_effect: Insert => "values-single-unused-effect",
    values_multiple_unused_effect: Insert => "values-multiple-unused-effect",
    values_single_unused_error: Insert => "values-single-unused-error",
    values_multiple_unused_error: Insert => "values-multiple-unused-error",
    values_single_default: Insert => "values-single-default",
    values_multiple_default: Insert => "values-multiple-default",
    values_nested: Insert => "values-nested",
    values_nested_condition: Insert => "values-nested-condition",
    values_prepared: Insert => "values-prepared",
    values_view_omitted_default: Insert => "values-view-omitted-default",
    values_view_explicit_default: Insert => "values-view-explicit-default",
    values_view_multiple_defaults: Insert => "values-view-multiple-defaults",
    values_partial_single: Insert => "values-partial-single",
    values_partial_multiple: Insert => "values-partial-multiple",
    values_domain_single: Insert => "values-domain-single",
    values_domain_multiple: Insert => "values-domain-multiple",
    values_discarded_input: Insert => "values-discarded-input",
    query_discarded_explicit_columns: Insert => "query-discarded-explicit-columns",
    values_action_default: Insert => "values-action-default",
    values_action_multiple_defaults: Insert => "values-action-multiple-defaults",
    values_explicit_default_values: Insert => "values-explicit-default-values",
    values_column_collation: Insert => "values-column-collation",
    values_explicit_collation: Insert => "values-explicit-collation",
    query_explicit_collation: Insert => "query-explicit-collation",
    values_action_default_view: Insert => "values-action-default-view",
    values_action_mixed_default_view: Insert => "values-action-mixed-default-view",
    values_action_default_view_rules: Insert => "values-action-default-view-rules",
    values_action_mixed_default_view_rules: Insert => "values-action-mixed-default-view-rules",
    values_action_default_rule_chain: Insert => "values-action-default-rule-chain",
    identity_default_single: Insert => "identity-default-single",
    identity_default_multiple: Insert => "identity-default-multiple",
    identity_omitted: Insert => "identity-omitted",
    identity_override_single: Insert => "identity-override-single",
    identity_override_multiple: Insert => "identity-override-multiple",
    domain_default_single: Insert => "domain-default-single",
    domain_default_multiple: Insert => "domain-default-multiple",
    domain_default_omitted_query: Insert => "domain-default-omitted-query",
    action_identity_always_default: Insert => "action-identity-always-default",
    action_identity_always_mixed: Insert => "action-identity-always-mixed",
    identity_mixed_default: Insert => "identity-mixed-default",
    identity_override_default_single: Insert => "identity-override-default-single",
    identity_override_default_multiple: Insert => "identity-override-default-multiple",
    identity_override_query: Insert => "identity-override-query",
    identity_override_values_effect: Insert => "identity-override-values-effect",
    identity_also_single: Insert => "identity-also-single",
    identity_also_multiple: Insert => "identity-also-multiple",
    identity_also_override_default: Insert => "identity-also-override-default",
    identity_always_view_default: Insert => "identity-always-view-default",
    identity_always_view_written: Insert => "identity-always-view-written",
    action_generated_default: Insert => "action-generated-default",
    action_generated_mixed: Insert => "action-generated-mixed",
    identity_view_override_default: Insert => "identity-view-override-default",
    identity_view_override_written: Insert => "identity-view-override-written",
    set_action_single: Insert => "set-action-single",
    set_action_multiple: Insert => "set-action-multiple",
    set_action_query: Insert => "set-action-query",
    set_action_empty_query: Insert => "set-action-empty-query",
    set_action_error_before_input: Insert => "set-action-error-before-input",
    set_action_error_before_trigger: Insert => "set-action-error-before-trigger",
    set_action_nested_error_before_input: Insert => "set-action-nested-error-before-input",
    recursive_conflict_before_input: Insert => "recursive-conflict-before-input",
    set_action_default_values: Insert => "set-action-default-values",
    ordinary_insert_unused_cte: Insert => "ordinary-insert-unused-cte",
    ordinary_update_unused_cte: Insert => "ordinary-update-unused-cte",
    ordinary_delete_unused_cte: Insert => "ordinary-delete-unused-cte",
    ordinary_merge_unused_cte: Insert => "ordinary-merge-unused-cte",
    ordinary_unused_volatile_cte: Insert => "ordinary-unused-volatile-cte",
    ordinary_cte_write_dependency: Insert => "ordinary-cte-write-dependency",
    ordinary_cte_unused_chain: Insert => "ordinary-cte-unused-chain",
    ordinary_cte_returning_reference: Insert => "ordinary-cte-returning-reference",
    ordinary_cte_referenced_error: Insert => "ordinary-cte-referenced-error",
    unused_derived_scalar_error: Insert => "unused-derived-scalar-error",
    unused_derived_scalar_volatile: Insert => "unused-derived-scalar-volatile",
    values_srf_also_values_single_series: Insert => "values-srf-also-values-single-series",
    values_srf_also_values_single_series_returning: Insert => "values-srf-also-values-single-series-returning",
    values_srf_also_values_single_unnest: Insert => "values-srf-also-values-single-unnest",
    values_srf_also_values_single_unnest_returning: Insert => "values-srf-also-values-single-unnest-returning",
    values_srf_also_values_multiple: Insert => "values-srf-also-values-multiple",
    values_srf_also_values_multiple_returning: Insert => "values-srf-also-values-multiple-returning",
    values_srf_also_values_query: Insert => "values-srf-also-values-query",
    values_srf_also_values_query_returning: Insert => "values-srf-also-values-query-returning",
    values_srf_also_select_single_series: Insert => "values-srf-also-select-single-series",
    values_srf_also_select_single_series_returning: Insert => "values-srf-also-select-single-series-returning",
    values_srf_also_select_single_unnest: Insert => "values-srf-also-select-single-unnest",
    values_srf_also_select_single_unnest_returning: Insert => "values-srf-also-select-single-unnest-returning",
    values_srf_also_select_multiple: Insert => "values-srf-also-select-multiple",
    values_srf_also_select_multiple_returning: Insert => "values-srf-also-select-multiple-returning",
    values_srf_also_select_query: Insert => "values-srf-also-select-query",
    values_srf_also_select_query_returning: Insert => "values-srf-also-select-query-returning",
    values_srf_conditional_also_single_series: Insert => "values-srf-conditional-also-single-series",
    values_srf_conditional_also_single_series_returning: Insert => "values-srf-conditional-also-single-series-returning",
    values_srf_conditional_also_single_unnest: Insert => "values-srf-conditional-also-single-unnest",
    values_srf_conditional_also_single_unnest_returning: Insert => "values-srf-conditional-also-single-unnest-returning",
    values_srf_conditional_also_multiple: Insert => "values-srf-conditional-also-multiple",
    values_srf_conditional_also_multiple_returning: Insert => "values-srf-conditional-also-multiple-returning",
    values_srf_conditional_also_query: Insert => "values-srf-conditional-also-query",
    values_srf_conditional_also_query_returning: Insert => "values-srf-conditional-also-query-returning",
    values_srf_conditional_instead_single_series: Insert => "values-srf-conditional-instead-single-series",
    values_srf_conditional_instead_single_series_returning: Insert => "values-srf-conditional-instead-single-series-returning",
    values_srf_conditional_instead_single_unnest: Insert => "values-srf-conditional-instead-single-unnest",
    values_srf_conditional_instead_single_unnest_returning: Insert => "values-srf-conditional-instead-single-unnest-returning",
    values_srf_conditional_instead_multiple: Insert => "values-srf-conditional-instead-multiple",
    values_srf_conditional_instead_multiple_returning: Insert => "values-srf-conditional-instead-multiple-returning",
    values_srf_conditional_instead_query: Insert => "values-srf-conditional-instead-query",
    values_srf_conditional_instead_query_returning: Insert => "values-srf-conditional-instead-query-returning",
    values_srf_instead_single_series: Insert => "values-srf-instead-single-series",
    values_srf_instead_single_series_returning: Insert => "values-srf-instead-single-series-returning",
    values_srf_instead_single_unnest: Insert => "values-srf-instead-single-unnest",
    values_srf_instead_single_unnest_returning: Insert => "values-srf-instead-single-unnest-returning",
    values_srf_instead_multiple: Insert => "values-srf-instead-multiple",
    values_srf_instead_multiple_returning: Insert => "values-srf-instead-multiple-returning",
    values_srf_instead_query: Insert => "values-srf-instead-query",
    values_srf_instead_query_returning: Insert => "values-srf-instead-query-returning",
    values_srf_action_reference: Insert => "values-srf-action-reference",
    values_srf_condition_reference: Insert => "values-srf-condition-reference",
    values_srf_update_action: Insert => "values-srf-update-action",
    values_srf_empty: Insert => "values-srf-empty",
    values_srf_nested: Insert => "values-srf-nested",
    values_srf_custom: Insert => "values-srf-custom",
    values_srf_unused_scalar_error: Insert => "values-srf-unused-scalar-error",
    values_srf_single_unused_error: Insert => "values-srf-single-unused-error",
    cte_write_rule_nothing: Insert => "cte-write-rule-nothing",
    cte_write_rule_also: Insert => "cte-write-rule-also",
    cte_write_rule_conditional: Insert => "cte-write-rule-conditional",
    cte_write_rule_multiple: Insert => "cte-write-rule-multiple",
    update_repeated_new: UpdateDelete => "update-repeated-new",
    update_independent_actions: UpdateDelete => "update-independent-actions",
    update_unused_error: UpdateDelete => "update-unused-error",
    update_unused_assignment: UpdateDelete => "update-unused-assignment",
    update_new_constant: UpdateDelete => "update-new-constant",
    update_constant: UpdateDelete => "update-constant",
    update_false: UpdateDelete => "update-false",
    update_target_predicate: UpdateDelete => "update-target-predicate",
    update_independent_predicate: UpdateDelete => "update-independent-predicate",
    update_from_constant: UpdateDelete => "update-from-constant",
    update_from_new: UpdateDelete => "update-from-new",
    update_from_old: UpdateDelete => "update-from-old",
    update_scalar_new: UpdateDelete => "update-scalar-new",
    update_assignment_default: UpdateDelete => "update-assignment-default",
    update_whole_row: UpdateDelete => "update-whole-row",
    update_also: UpdateDelete => "update-also",
    delete_repeated_old: UpdateDelete => "delete-repeated-old",
    delete_constant: UpdateDelete => "delete-constant",
    delete_target_predicate: UpdateDelete => "delete-target-predicate",
    delete_from_constant: UpdateDelete => "delete-from-constant",
    delete_from_old: UpdateDelete => "delete-from-old",
    delete_also: UpdateDelete => "delete-also",
    update_joined_qualification: UpdateDelete => "update-joined-qualification",
    delete_joined_qualification: UpdateDelete => "delete-joined-qualification",
    update_new_unchanged: UpdateDelete => "update-new-unchanged",
    update_scalar_local_shadow: UpdateDelete => "update-scalar-local-shadow",
    update_scalar_qualified_shadow: UpdateDelete => "update-scalar-qualified-shadow",
    update_grouped_assignment: UpdateDelete => "update-grouped-assignment",
    update_array_assignment: UpdateDelete => "update-array-assignment",
    update_array_multiple_assignment: UpdateDelete => "update-array-multiple-assignment",
    update_conditional_new: UpdateDelete => "update-conditional-new",
    delete_independent_actions: UpdateDelete => "delete-independent-actions",
    delete_constant_empty: UpdateDelete => "delete-constant-empty",
    update_constant_empty: UpdateDelete => "update-constant-empty",
    update_authority_event_old: UpdateDelete => "update-authority-event-old",
    update_authority_original_read: UpdateDelete => "update-authority-original-read",
    delete_authority_event_old: UpdateDelete => "delete-authority-event-old",
    delete_authority_original_read: UpdateDelete => "delete-authority-original-read",
    update_authority_from_caller: UpdateDelete => "update-authority-from-caller",
    update_authority_from_owner: UpdateDelete => "update-authority-from-owner",
    update_authority_subquery_caller: UpdateDelete => "update-authority-subquery-caller",
    update_authority_subquery_owner: UpdateDelete => "update-authority-subquery-owner",
    update_collation_new: UpdateDelete => "update-collation-new",
    update_collation_old: UpdateDelete => "update-collation-old",
    update_collation_subquery: UpdateDelete => "update-collation-subquery",
    update_collation_condition: UpdateDelete => "update-collation-condition",
    update_collation_null: UpdateDelete => "update-collation-null",
    delete_collation_old: UpdateDelete => "delete-collation-old",
    update_target_row_field: UpdateDelete => "update-target-row-field",
    update_target_row_alias: UpdateDelete => "update-target-row-alias",
    update_target_row_subquery: UpdateDelete => "update-target-row-subquery",
    update_target_whole_row: UpdateDelete => "update-target-whole-row",
    update_source_whole_row: UpdateDelete => "update-source-whole-row",
    delete_target_whole_row_predicate: UpdateDelete => "delete-target-whole-row-predicate",
    update_system_unqualified: UpdateDelete => "update-system-unqualified",
    update_system_qualified: UpdateDelete => "update-system-qualified",
    update_system_subquery: UpdateDelete => "update-system-subquery",
    update_source_system: UpdateDelete => "update-source-system",
    delete_system_unqualified: UpdateDelete => "delete-system-unqualified",
    delete_system_qualified: UpdateDelete => "delete-system-qualified",
    delete_source_system: UpdateDelete => "delete-source-system",
    update_view_lazy_nested: UpdateDelete => "update-view-lazy-nested",
    update_view_lazy_filtered: UpdateDelete => "update-view-lazy-filtered",
    update_view_lazy_aliased: UpdateDelete => "update-view-lazy-aliased",
    update_view_lazy_barrier: UpdateDelete => "update-view-lazy-barrier",
    update_view_authority_denied: UpdateDelete => "update-view-authority-denied",
    update_view_authority_column: UpdateDelete => "update-view-authority-column",
    update_view_authority_allowed: UpdateDelete => "update-view-authority-allowed",
    update_view_tableoid: UpdateDelete => "update-view-tableoid",
    update_view_xmin: UpdateDelete => "update-view-xmin",
    update_joined_assignment_every_candidate: UpdateDelete => "update-joined-assignment-every-candidate",
    update_joined_later_assignment_error: UpdateDelete => "update-joined-later-assignment-error",
    update_joined_trigger_skips_first: UpdateDelete => "update-joined-trigger-skips-first",
    update_joined_trigger_skips_all: UpdateDelete => "update-joined-trigger-skips-all",
    delete_joined_trigger_skips_first: UpdateDelete => "delete-joined-trigger-skips-first",
    delete_joined_trigger_skips_all: UpdateDelete => "delete-joined-trigger-skips-all",
    update_joined_assignment_before_skipped_trigger: UpdateDelete => "update-joined-assignment-before-skipped-trigger",
    update_nested_mutation_input: UpdateDelete => "update-nested-mutation-input",
    delete_nested_mutation_input: UpdateDelete => "delete-nested-mutation-input",
    update_nested_mutation_returning_false: UpdateDelete => "update-nested-mutation-returning-False",
    update_nested_mutation_returning_true: UpdateDelete => "update-nested-mutation-returning-True",
    delete_nested_mutation_returning_false: UpdateDelete => "delete-nested-mutation-returning-False",
    delete_nested_mutation_returning_true: UpdateDelete => "delete-nested-mutation-returning-True",
}

fn verify_case(provider: Provider, reference: Reference, name: &str) {
    let reference = reference.read();
    let case = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("missing PostgreSQL reference {name}"));
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rules.db"));
    engine.sql(case["setup"].as_str().unwrap(), &[]).unwrap();
    let recover = case["recover"].as_bool().unwrap_or(false);
    if recover {
        engine.sql("BEGIN; SAVEPOINT before_rule", &[]).unwrap();
    }
    let result = engine.sql(case["sql"].as_str().unwrap(), &[]);
    if let Some(state) = case["sqlstate"].as_str() {
        assert_eq!(
            result.as_ref().err().and_then(|error| error.sqlstate()),
            Some(state),
            "{name}: {result:?}"
        );
        if recover {
            engine.sql("ROLLBACK TO before_rule", &[]).unwrap();
        } else {
            return;
        }
    } else {
        result.unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let result = engine.sql(case["query"].as_str().unwrap(), &[]).unwrap();
    let Value::Json(text) = result.value_at(0, 0).unwrap() else {
        panic!("JSON result")
    };
    let observed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(observed, case["result"], "{name}");
    if recover {
        engine.sql("ROLLBACK", &[]).unwrap();
    }
}
