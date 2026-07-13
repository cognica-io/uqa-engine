//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coverage for learner-level cases in `test_parameter_learning`.

use uqa_scoring::ParameterLearner;

#[test]
fn init_default_params() {
    let params = ParameterLearner::default().params();
    assert_eq!(params["alpha"], 1.0);
    assert_eq!(params["beta"], 0.0);
    assert_eq!(params["base_rate"], 0.5);
}

#[test]
fn init_custom_params() {
    let params = ParameterLearner::new(2.0, 0.5, Some(0.3)).params();
    assert_eq!(params["alpha"], 2.0);
    assert_eq!(params["beta"], 0.5);
    assert_eq!(params["base_rate"], 0.3);
}

#[test]
fn fit_returns_params() {
    let mut learner = ParameterLearner::default();
    let result = learner.fit_with_options(&[0.1, 0.3, 0.5, 0.7, 0.9], &[0.0, 0.0, 0.0, 1.0, 1.0]);
    assert!(result.contains_key("alpha"));
    assert!(result.contains_key("beta"));
    assert!(result.contains_key("base_rate"));
}

#[test]
fn fit_changes_params() {
    let mut learner = ParameterLearner::default();
    let initial = learner.params();
    let fitted_params = learner.fit_with_options(&[0.1, 0.2, 0.8, 0.9], &[0.0, 0.0, 1.0, 1.0]);
    assert!(
        (fitted_params["alpha"] - initial["alpha"]).abs() > 1e-6
            || (fitted_params["beta"] - initial["beta"]).abs() > 1e-6
            || (fitted_params["base_rate"] - initial["base_rate"]).abs() > 1e-6
    );
}

#[test]
fn fit_with_mode_shape() {
    let mut learner = ParameterLearner::default();
    let result = learner.fit_with_options(&[0.1, 0.3, 0.7, 0.9], &[0.0, 0.0, 1.0, 1.0]);
    assert!(result.contains_key("alpha"));
}

#[test]
fn fit_uses_complete_raw_query_scores() {
    let mut learner = ParameterLearner::default();
    let result = learner.fit_with_options(&[0.1, 0.3, 0.7, 0.9], &[0.0, 0.0, 1.0, 1.0]);
    assert!(result.contains_key("alpha"));
}

#[test]
fn update_modifies_params() {
    let mut learner = ParameterLearner::default();
    let initial = learner.params();
    for _ in 0..100 {
        learner.update(0.9, 1.0, 0.1);
        learner.update(0.1, 0.0, 0.1);
    }
    let updated = learner.params();
    assert!(
        (updated["alpha"] - initial["alpha"]).abs() > 1e-6
            || (updated["beta"] - initial["beta"]).abs() > 1e-6
    );
}

#[test]
fn update_accepts_a_raw_query_score() {
    let mut learner = ParameterLearner::default();
    learner.update(0.5, 1.0, 0.1);
    assert!(learner.params().contains_key("alpha"));
}

#[test]
fn params_returns_floats() {
    let params = ParameterLearner::default().params();
    for key in ["alpha", "beta", "base_rate"] {
        assert!(params[key].is_finite());
    }
}
