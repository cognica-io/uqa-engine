//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::{Deserialize, Serialize};

use crate::backend::{try_filled_vec, try_vec_with_capacity, MLError, MLResult};
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
    let (dims, class_count) = validate_training_shape(training_set, options)?;
    let (counts, sums) = accumulate_training_classes(training_set, dims, class_count)?;
    let (weights, bias) = classifier_parameters(training_set.examples.len(), &counts, &sums)?;

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

fn validate_training_shape(
    training_set: &TrainingSet,
    options: &LearnOptions,
) -> MLResult<(usize, usize)> {
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
    for (row, example) in training_set.examples.iter().enumerate() {
        if let Some((column, value)) = example
            .features
            .iter()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(MLError::InvalidTrainingSet(format!(
                "training feature [{row}][{column}] must be finite, got {value}"
            )));
        }
    }
    if !options.alpha.is_finite() {
        return Err(MLError::InvalidTrainingSet(format!(
            "training alpha must be finite, got {}",
            options.alpha
        )));
    }
    let inferred_classes = training_set
        .examples
        .iter()
        .map(|example| example.label)
        .max()
        .map(|label| {
            label.checked_add(1).ok_or_else(|| {
                MLError::InvalidTrainingSet("training label exceeds the usize range".into())
            })
        })
        .transpose()?
        .unwrap_or(0);
    let class_count = training_set.class_count.unwrap_or(inferred_classes);
    if class_count == 0 {
        return Err(MLError::InvalidTrainingSet(
            "deep_learn requires at least one class".into(),
        ));
    }
    if class_count > training_set.examples.len() {
        return Err(MLError::InvalidTrainingSet(format!(
            "class_count={class_count} exceeds the number of training examples and necessarily contains an empty class"
        )));
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

    class_count.checked_mul(dims).ok_or_else(|| {
        MLError::InvalidTrainingSet("training matrix dimensions overflow usize".into())
    })?;
    Ok((dims, class_count))
}

fn accumulate_training_classes(
    training_set: &TrainingSet,
    dims: usize,
    class_count: usize,
) -> MLResult<(Vec<usize>, Vec<Vec<f64>>)> {
    let mut counts = try_filled_vec(class_count, 0usize, "training class counts")?;
    let mut sums = try_vec_with_capacity(class_count, "training class feature sums")?;
    for class in 0..class_count {
        sums.push(try_filled_vec(
            dims,
            0.0f64,
            &format!("training feature sums for class {class}"),
        )?);
    }
    for example in &training_set.examples {
        counts[example.label] = counts[example.label].checked_add(1).ok_or_else(|| {
            MLError::InvalidTrainingSet(format!(
                "training example count for class {} overflows usize",
                example.label
            ))
        })?;
        for (i, value) in example.features.iter().enumerate() {
            let sum = sums[example.label][i] + value;
            if !sum.is_finite() {
                return Err(MLError::InvalidTrainingSet(format!(
                    "training feature sum for class {}, column {i} is non-finite",
                    example.label
                )));
            }
            sums[example.label][i] = sum;
        }
    }
    if let Some(empty_class) = counts.iter().position(|count| *count == 0) {
        return Err(MLError::InvalidTrainingSet(format!(
            "class {empty_class} has no training examples"
        )));
    }
    Ok((counts, sums))
}

fn classifier_parameters(
    example_count: usize,
    counts: &[usize],
    sums: &[Vec<f64>],
) -> MLResult<(Vec<f64>, Vec<f64>)> {
    let dims = sums.first().map_or(0, Vec::len);
    let weight_count = counts.len().checked_mul(dims).ok_or_else(|| {
        MLError::InvalidTrainingSet("trained weight count overflows usize".into())
    })?;
    let mut weights = try_vec_with_capacity(weight_count, "trained classifier weights")?;
    let mut bias = try_vec_with_capacity(counts.len(), "trained classifier bias")?;
    let total = usize_to_f64_exact(example_count, "training example count")?;
    for class in 0..counts.len() {
        let class_examples = usize_to_f64_exact(counts[class], "class example count")?;
        let inv_count = 1.0 / class_examples;
        let mut centroid = try_vec_with_capacity(dims, "training class centroid")?;
        centroid.extend(sums[class].iter().map(|value| value * inv_count));
        let norm_sq: f64 = centroid.iter().map(|value| value * value).sum();
        if !norm_sq.is_finite() {
            return Err(MLError::InvalidTrainingSet(format!(
                "centroid norm for class {class} is non-finite"
            )));
        }
        weights.extend_from_slice(&centroid);
        let class_bias = -0.5 * norm_sq + (class_examples / total).ln();
        if !class_bias.is_finite() {
            return Err(MLError::InvalidTrainingSet(format!(
                "trained bias for class {class} is non-finite"
            )));
        }
        bias.push(class_bias);
    }
    Ok((weights, bias))
}

fn usize_to_f64_exact(value: usize, context: &str) -> MLResult<f64> {
    const MAX_EXACT_INTEGER: u64 = 9_007_199_254_740_992;
    let value = u64::try_from(value)
        .map_err(|_| MLError::InvalidTrainingSet(format!("{context} exceeds the u64 bridge")))?;
    if value > MAX_EXACT_INTEGER {
        return Err(MLError::InvalidTrainingSet(format!(
            "{context} exceeds f64's exact integer range"
        )));
    }
    Ok(value as f64)
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

    #[test]
    fn training_rejects_non_finite_features_and_alpha() {
        let non_finite_feature = TrainingSet {
            examples: vec![TrainingExample {
                features: vec![f64::INFINITY],
                label: 0,
            }],
            class_count: Some(1),
        };
        let error = deep_learn(&non_finite_feature, &LearnOptions::default())
            .expect_err("non-finite input must be rejected");
        assert!(error.to_string().contains("must be finite"));

        let valid = TrainingSet {
            examples: vec![TrainingExample {
                features: vec![1.0],
                label: 0,
            }],
            class_count: Some(1),
        };
        let error = deep_learn(
            &valid,
            &LearnOptions {
                alpha: f64::NAN,
                ..LearnOptions::default()
            },
        )
        .expect_err("non-finite alpha must be rejected");
        assert!(error.to_string().contains("alpha must be finite"));
    }

    #[test]
    fn impossible_class_counts_fail_before_allocation() {
        let training_set = TrainingSet {
            examples: vec![TrainingExample {
                features: vec![1.0],
                label: 0,
            }],
            class_count: Some(usize::MAX),
        };
        let error = deep_learn(&training_set, &LearnOptions::default())
            .expect_err("an impossible class count must not trigger a huge allocation");
        assert!(error
            .to_string()
            .contains("exceeds the number of training examples"));
    }
}
