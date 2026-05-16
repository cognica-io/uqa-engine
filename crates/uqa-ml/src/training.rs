//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use crate::backend::{MLError, MLResult};
use crate::model::{DeepLayerSpec, DeepModel, GatingSpec};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrainingExample {
    pub features: Vec<f64>,
    pub label: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrainingSet {
    pub examples: Vec<TrainingExample>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearnOptions {
    #[serde(default)]
    pub alpha: f64,
    #[serde(default)]
    pub gating: GatingSpec,
}

impl Default for LearnOptions {
    fn default() -> Self {
        Self {
            alpha: 0.0,
            gating: GatingSpec::None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrainingReport {
    pub examples: usize,
    pub feature_dimensions: usize,
    pub class_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeepLearnOutput {
    pub model: DeepModel,
    pub report: TrainingReport,
}

/// Analytical nearest-centroid softmax training.
///
/// For each class this estimates the class centroid and emits a single
/// dense-softmax classifier with logits equivalent to the linear part of
/// negative squared Euclidean distance plus log-prior:
///
/// `logit_c(x) = centroid_c dot x - 0.5 * ||centroid_c||^2 + log prior_c`.
pub fn deep_learn(training_set: &TrainingSet, options: &LearnOptions) -> MLResult<DeepLearnOutput> {
    let Some(first) = training_set.examples.first() else {
        return Err(MLError::InvalidTrainingSet(
            "deep_learn requires at least one training example".into(),
        ));
    };
    let dims = first.features.len();
    if dims == 0 {
        return Err(MLError::InvalidTrainingSet(
            "deep_learn requires non-empty feature vectors".into(),
        ));
    }
    if training_set
        .examples
        .iter()
        .any(|example| example.features.len() != dims)
    {
        return Err(MLError::InvalidTrainingSet(
            "deep_learn requires all feature vectors to have the same dimension".into(),
        ));
    }
    let inferred_classes = training_set
        .examples
        .iter()
        .map(|example| example.label)
        .max()
        .map_or(0, |label| label + 1);
    let class_count = training_set.class_count.unwrap_or(inferred_classes);
    if class_count == 0 {
        return Err(MLError::InvalidTrainingSet(
            "deep_learn requires at least one class".into(),
        ));
    }
    if training_set
        .examples
        .iter()
        .any(|example| example.label >= class_count)
    {
        return Err(MLError::InvalidTrainingSet(format!(
            "training label is outside class_count={class_count}"
        )));
    }

    let mut counts = vec![0usize; class_count];
    let mut sums = vec![vec![0.0f64; dims]; class_count];
    for example in &training_set.examples {
        counts[example.label] += 1;
        for (i, value) in example.features.iter().enumerate() {
            sums[example.label][i] += value;
        }
    }
    if let Some(empty_class) = counts.iter().position(|count| *count == 0) {
        return Err(MLError::InvalidTrainingSet(format!(
            "class {empty_class} has no training examples"
        )));
    }

    let mut weights = Vec::with_capacity(class_count * dims);
    let mut bias = Vec::with_capacity(class_count);
    let total = training_set.examples.len() as f64;
    for class in 0..class_count {
        let inv_count = 1.0 / counts[class] as f64;
        let centroid: Vec<f64> = sums[class].iter().map(|value| value * inv_count).collect();
        let norm_sq: f64 = centroid.iter().map(|value| value * value).sum();
        weights.extend_from_slice(&centroid);
        bias.push(-0.5 * norm_sq + (counts[class] as f64 / total).ln());
    }

    let model = DeepModel {
        layers: vec![
            DeepLayerSpec::Input { dimensions: dims },
            DeepLayerSpec::Dense {
                weights,
                bias,
                output_channels: class_count,
                input_channels: dims,
            },
            DeepLayerSpec::Softmax,
        ],
        alpha: options.alpha,
        gating: options.gating,
    };
    Ok(DeepLearnOutput {
        model,
        report: TrainingReport {
            examples: training_set.examples.len(),
            feature_dimensions: dims,
            class_count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CPUBackend, MLBackend};

    #[test]
    fn centroid_training_separates_two_classes() {
        let training_set = TrainingSet {
            examples: vec![
                TrainingExample {
                    features: vec![2.0, 0.0],
                    label: 0,
                },
                TrainingExample {
                    features: vec![3.0, 0.0],
                    label: 0,
                },
                TrainingExample {
                    features: vec![0.0, 2.0],
                    label: 1,
                },
                TrainingExample {
                    features: vec![0.0, 3.0],
                    label: 1,
                },
            ],
            class_count: None,
        };
        let output = deep_learn(&training_set, &LearnOptions::default()).unwrap();
        assert_eq!(output.report.class_count, 2);

        let backend = CPUBackend;
        let (_, probs) = backend
            .predict_features(&output.model, &[(1, vec![4.0, 0.0]), (2, vec![0.0, 4.0])])
            .unwrap();
        assert!(probs[&1][0] > probs[&1][1], "{probs:?}");
        assert!(probs[&2][1] > probs[&2][0], "{probs:?}");
    }
}
