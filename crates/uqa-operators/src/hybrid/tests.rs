use super::*;

struct LiteralOperator(Vec<(DocId, f64)>);

impl Operator for LiteralOperator {
    fn execute(&self, _ctx: &ExecutionContext) -> OperatorResult {
        Ok(PostingList::from_sorted_unchecked(
            self.0
                .iter()
                .map(|(doc_id, score)| PostingEntry::new(*doc_id, Payload::with_score(*score)))
                .collect(),
        ))
    }

    fn cost_estimate(&self, _stats: &IndexStats) -> f64 {
        self.0.len() as f64
    }
}

#[test]
fn exact_bayesian_operator_keeps_neutral_evidence_at_the_prior() {
    let signals: Vec<Arc<dyn Operator>> = vec![
        Arc::new(LiteralOperator(vec![(1, 0.5), (2, 0.8)])),
        Arc::new(LiteralOperator(vec![(1, 0.5), (2, 0.25)])),
    ];
    let result = BayesianEvidenceFusionOperator::new(signals, 0.1)
        .execute(&ExecutionContext::new())
        .unwrap();
    let neutral = result.get_entry(1).unwrap().payload.score;
    assert!((neutral - 0.1).abs() < 1e-12, "got {neutral}");

    let expected = uqa_scoring::sigmoid(
        uqa_scoring::logit(0.1) + uqa_scoring::logit(0.8) + uqa_scoring::logit(0.25),
    );
    let signed = result.get_entry(2).unwrap().payload.score;
    assert!((signed - expected).abs() < 1e-12, "{signed} != {expected}");
}

#[test]
fn exact_bayesian_operator_rejects_invalid_priors_and_signal_scores() {
    let invalid_prior =
        BayesianEvidenceFusionOperator::new(vec![Arc::new(LiteralOperator(vec![(1, 0.5)]))], 0.0)
            .execute(&ExecutionContext::new())
            .unwrap_err();
    assert!(invalid_prior.to_string().contains("base_rate"));

    let invalid_score =
        BayesianEvidenceFusionOperator::new(vec![Arc::new(LiteralOperator(vec![(1, 1.1)]))], 0.5)
            .execute(&ExecutionContext::new())
            .unwrap_err();
    assert!(invalid_score.to_string().contains("probability"));
}

#[test]
fn adaptive_weights_favor_the_discriminating_signal() {
    let fuser = RobustPositiveEvidencePool::new(0.5).expect("test alpha is valid");
    let flat: BTreeMap<DocId, f64> = [(1, 0.7), (2, 0.7), (3, 0.7)].into_iter().collect();
    let spread: BTreeMap<DocId, f64> = [(1, 0.9), (2, 0.5), (3, 0.1)].into_iter().collect();
    let weights = adaptive_signal_weights(&fuser, &[flat.clone(), spread])
        .expect("spread signal yields weights");
    assert!(
        weights[1] > weights[0],
        "discriminating signal must outweigh the flat one: {weights:?}"
    );
    assert!((weights.iter().sum::<f64>() - 1.0).abs() < 1e-9);
    assert!(
        adaptive_signal_weights(&fuser, &[flat.clone(), flat]).is_none(),
        "all-flat signals fall back to the unweighted mean"
    );
}

#[test]
fn adaptive_operator_wires_gating_to_the_fuser() {
    let signals: Vec<Arc<dyn Operator>> = vec![
        Arc::new(LiteralOperator(vec![(1, 0.2)])),
        Arc::new(LiteralOperator(vec![(1, 0.3)])),
    ];
    let softplus = AdaptivePositiveEvidencePoolOperator::new(signals.clone(), 0.5, None)
        .execute(&ExecutionContext::new())
        .unwrap();
    let pass = AdaptivePositiveEvidencePoolOperator::new(signals, 0.5, Some("pass".into()))
        .execute(&ExecutionContext::new())
        .unwrap();
    let softplus_score = softplus.entries()[0].payload.score;
    let pass_score = pass.entries()[0].payload.score;
    assert!(
        softplus_score > 0.5,
        "softplus floors weak evidence, got {softplus_score}"
    );
    assert!(
        pass_score < 0.5,
        "pass gating lets weak evidence sink, got {pass_score}"
    );
}

fn two_literal_signals() -> Vec<Arc<dyn Operator>> {
    vec![
        Arc::new(LiteralOperator(vec![(1, 0.8)])),
        Arc::new(LiteralOperator(vec![(1, 0.7)])),
    ]
}

#[test]
fn malformed_positive_evidence_configuration_returns_operator_error() {
    let context = ExecutionContext::new();
    let cases = [
        RobustPositiveEvidencePoolOperator::new(two_literal_signals(), f64::NAN),
        RobustPositiveEvidencePoolOperator::new(two_literal_signals(), 0.5)
            .with_weights(vec![0.8, 0.8]),
        RobustPositiveEvidencePoolOperator::new(two_literal_signals(), 0.5)
            .with_logit_normalization(vec![0.0, 1.0], vec![0.0, 2.0]),
    ];

    for operator in cases {
        let error = operator
            .execute(&context)
            .expect_err("malformed positive-evidence configuration must fail");
        assert!(
            error.to_string().contains("positive-evidence")
                || error.to_string().contains("weights")
                || error.to_string().contains("bounds"),
            "unexpected error: {error}"
        );
    }
}

#[test]
fn coverage_default_neutral_at_zero_coverage() {
    let d = coverage_based_default(0, 0, 0.01);
    assert!((d - 0.5).abs() < 1e-12);
}

#[test]
fn coverage_default_at_floor_at_full_coverage() {
    let d = coverage_based_default(100, 100, 0.01);
    assert!((d - 0.01).abs() < 1e-12);
}

#[test]
fn coverage_default_interpolates() {
    let d = coverage_based_default(50, 100, 0.01);
    // r = 0.5 -> 0.5 * 0.5 + 0.01 * 0.5 = 0.255
    assert!((d - 0.255).abs() < 1e-12);
}
