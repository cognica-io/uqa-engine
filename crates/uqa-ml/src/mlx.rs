//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Optional Apple MLX backend through the official `mlx-c` API.

#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::{c_float, c_int};
use std::ptr;
use std::slice;

use uqa_core::DocId;
use uqa_operators::ExecutionContext;

use crate::backend::{CPUBackend, MLBackend, MLError, MLResult};
use crate::model::{predict_cpu, DeepLayerSpec, DeepModel, PredictResult};
use crate::training::{deep_learn, DeepLearnOutput, LearnOptions, TrainingSet};

#[derive(Debug)]
pub struct MLXBackend {
    device: RawDevice,
    stream: RawStream,
}

impl MLXBackend {
    pub fn preferred() -> MLResult<Self> {
        let device = RawDevice::preferred()?;
        let stream = RawStream::new(&device);
        Ok(Self { device, stream })
    }

    pub fn cpu() -> Self {
        let device = RawDevice::cpu();
        let stream = RawStream::new(&device);
        Self { device, stream }
    }

    pub fn device_kind(&self) -> MLResult<&'static str> {
        self.device.kind()
    }
}

impl MLBackend for MLXBackend {
    fn name(&self) -> &'static str {
        "mlx"
    }

    fn predict(&self, model: &DeepModel, ctx: &ExecutionContext) -> MLResult<PredictResult> {
        Ok(predict_cpu(model, ctx))
    }

    fn predict_features(
        &self,
        model: &DeepModel,
        examples: &[(DocId, Vec<f64>)],
    ) -> MLResult<PredictResult> {
        if let Some(plan) = DenseSoftmaxPlan::from_model(model) {
            return self.predict_dense_softmax(plan, examples);
        }
        CPUBackend.predict_features(model, examples)
    }

    fn deep_learn(
        &self,
        training_set: &TrainingSet,
        options: &LearnOptions,
    ) -> MLResult<DeepLearnOutput> {
        deep_learn(training_set, options)
    }
}

impl MLXBackend {
    fn predict_dense_softmax(
        &self,
        plan: DenseSoftmaxPlan<'_>,
        examples: &[(DocId, Vec<f64>)],
    ) -> MLResult<PredictResult> {
        if examples.is_empty() {
            return Ok((Vec::new(), BTreeMap::default()));
        }
        let mut input = Vec::with_capacity(examples.len() * plan.input_channels);
        for (doc_id, features) in examples {
            if features.len() != plan.input_channels {
                return Err(MLError::InvalidModel(format!(
                    "feature vector for doc {doc_id} has dimension {}, expected {}",
                    features.len(),
                    plan.input_channels
                )));
            }
            input.extend(features.iter().map(|value| *value as f32));
        }

        let mut weights = Vec::with_capacity(plan.input_channels * plan.output_channels);
        for input_idx in 0..plan.input_channels {
            for output_idx in 0..plan.output_channels {
                weights.push(plan.weights[output_idx * plan.input_channels + input_idx] as f32);
            }
        }
        let bias: Vec<f32> = plan.bias.iter().map(|value| *value as f32).collect();

        let input = RawArray::from_f32(&[examples.len(), plan.input_channels], &input)?;
        let weights = RawArray::from_f32(&[plan.input_channels, plan.output_channels], &weights)?;
        let bias = RawArray::from_f32(&[plan.output_channels], &bias)?;

        let logits = input
            .matmul(&weights, &self.stream)?
            .add(&bias, &self.stream)?;
        let probs = logits.softmax_axis(1, &self.stream)?;
        let flat = probs.to_f32_vec(&self.stream)?;

        let mut scores = Vec::with_capacity(examples.len());
        let mut class_probs = std::collections::BTreeMap::new();
        for (row, (doc_id, _)) in examples.iter().enumerate() {
            let offset = row * plan.output_channels;
            let row_probs: Vec<f64> = flat[offset..offset + plan.output_channels]
                .iter()
                .map(|value| f64::from(*value))
                .collect();
            let score = row_probs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            scores.push((*doc_id, score));
            class_probs.insert(*doc_id, row_probs);
        }
        scores.sort_by_key(|(doc_id, _)| *doc_id);
        Ok((scores, class_probs))
    }
}

#[derive(Clone, Copy)]
struct DenseSoftmaxPlan<'a> {
    weights: &'a [f64],
    bias: &'a [f64],
    input_channels: usize,
    output_channels: usize,
}

impl<'a> DenseSoftmaxPlan<'a> {
    fn from_model(model: &'a DeepModel) -> Option<Self> {
        let [DeepLayerSpec::Input { dimensions }, DeepLayerSpec::Dense {
            weights,
            bias,
            output_channels,
            input_channels,
        }, DeepLayerSpec::Softmax] = model.layers.as_slice()
        else {
            return None;
        };
        if *dimensions != *input_channels
            || weights.len() != output_channels * input_channels
            || bias.len() != *output_channels
        {
            return None;
        }
        Some(Self {
            weights,
            bias,
            input_channels: *input_channels,
            output_channels: *output_channels,
        })
    }
}

#[derive(Debug)]
struct RawDevice {
    raw: raw::mlx_device,
}

impl RawDevice {
    fn preferred() -> MLResult<Self> {
        let gpu = Self {
            raw: unsafe { raw::mlx_device_new_type(raw::MLX_GPU, 0) },
        };
        if gpu.is_available()? {
            return Ok(gpu);
        }
        drop(gpu);
        Ok(Self::cpu())
    }

    fn cpu() -> Self {
        Self {
            raw: unsafe { raw::mlx_device_new_type(raw::MLX_CPU, 0) },
        }
    }

    fn is_available(&self) -> MLResult<bool> {
        let mut available = false;
        unsafe {
            check(
                raw::mlx_device_is_available(ptr::addr_of_mut!(available), self.raw),
                "mlx_device_is_available",
            )?;
        }
        Ok(available)
    }

    fn kind(&self) -> MLResult<&'static str> {
        let mut kind = raw::MLX_CPU;
        unsafe {
            check(
                raw::mlx_device_get_type(ptr::addr_of_mut!(kind), self.raw),
                "mlx_device_get_type",
            )?;
        }
        Ok(match kind {
            raw::MLX_GPU => "GPU",
            raw::MLX_CPU => "CPU",
            _ => "Unknown",
        })
    }
}

impl Drop for RawDevice {
    fn drop(&mut self) {
        unsafe {
            let _ = raw::mlx_device_free(self.raw);
        }
    }
}

#[derive(Debug)]
struct RawStream {
    raw: raw::mlx_stream,
}

impl RawStream {
    fn new(device: &RawDevice) -> Self {
        Self {
            raw: unsafe { raw::mlx_stream_new_device(device.raw) },
        }
    }

    fn synchronize(&self) -> MLResult<()> {
        unsafe { check(raw::mlx_synchronize(self.raw), "mlx_synchronize") }
    }
}

impl Drop for RawStream {
    fn drop(&mut self) {
        unsafe {
            let _ = raw::mlx_stream_free(self.raw);
        }
    }
}

#[derive(Debug)]
struct RawArray {
    raw: raw::mlx_array,
}

impl RawArray {
    fn empty() -> raw::mlx_array {
        raw::mlx_array {
            ctx: ptr::null_mut(),
        }
    }

    fn from_f32(shape: &[usize], values: &[f32]) -> MLResult<Self> {
        let expected: usize = shape.iter().product();
        if expected != values.len() {
            return Err(MLError::Backend(format!(
                "MLX array shape {shape:?} does not match {} values",
                values.len()
            )));
        }
        let shape: Vec<c_int> = shape.iter().map(|dim| *dim as c_int).collect();
        let raw = unsafe {
            raw::mlx_array_new_data(
                values.as_ptr().cast(),
                shape.as_ptr(),
                shape.len() as c_int,
                raw::MLX_FLOAT32,
            )
        };
        if raw.ctx.is_null() {
            return Err(MLError::Backend(
                "mlx_array_new_data returned a null handle".into(),
            ));
        }
        Ok(Self { raw })
    }

    fn matmul(&self, rhs: &Self, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        unsafe {
            check(
                raw::mlx_matmul(ptr::addr_of_mut!(out), self.raw, rhs.raw, stream.raw),
                "mlx_matmul",
            )?;
        }
        Ok(Self { raw: out })
    }

    fn add(&self, rhs: &Self, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        unsafe {
            check(
                raw::mlx_add(ptr::addr_of_mut!(out), self.raw, rhs.raw, stream.raw),
                "mlx_add",
            )?;
        }
        Ok(Self { raw: out })
    }

    fn softmax_axis(&self, axis: i32, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        unsafe {
            check(
                raw::mlx_softmax_axis(ptr::addr_of_mut!(out), self.raw, axis, true, stream.raw),
                "mlx_softmax_axis",
            )?;
        }
        Ok(Self { raw: out })
    }

    fn to_f32_vec(&self, stream: &RawStream) -> MLResult<Vec<f32>> {
        unsafe {
            check(raw::mlx_array_eval(self.raw), "mlx_array_eval")?;
            stream.synchronize()?;
            let count = raw::mlx_array_size(self.raw);
            let ptr = raw::mlx_array_data_float32(self.raw);
            if ptr.is_null() {
                return Err(MLError::Backend(
                    "mlx_array_data_float32 returned null".into(),
                ));
            }
            Ok(slice::from_raw_parts(ptr, count).to_vec())
        }
    }
}

impl Drop for RawArray {
    fn drop(&mut self) {
        unsafe {
            let _ = raw::mlx_array_free(self.raw);
        }
    }
}

fn check(code: c_int, context: &str) -> MLResult<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(MLError::Backend(format!(
            "{context} failed with MLX error code {code}"
        )))
    }
}

#[allow(non_camel_case_types)]
mod raw {
    use super::{c_float, c_int, c_void};

    pub type mlx_dtype = c_int;
    pub const MLX_FLOAT32: mlx_dtype = 10;

    pub type mlx_device_type = c_int;
    pub const MLX_CPU: mlx_device_type = 0;
    pub const MLX_GPU: mlx_device_type = 1;

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct mlx_array {
        pub ctx: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct mlx_device {
        pub ctx: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct mlx_stream {
        pub ctx: *mut c_void,
    }

    unsafe extern "C" {
        pub fn mlx_array_new_data(
            data: *const c_void,
            shape: *const c_int,
            dim: c_int,
            dtype: mlx_dtype,
        ) -> mlx_array;
        pub fn mlx_array_free(arr: mlx_array) -> c_int;
        pub fn mlx_array_eval(arr: mlx_array) -> c_int;
        pub fn mlx_array_size(arr: mlx_array) -> usize;
        pub fn mlx_array_data_float32(arr: mlx_array) -> *const c_float;

        pub fn mlx_device_new_type(type_: mlx_device_type, index: c_int) -> mlx_device;
        pub fn mlx_device_free(dev: mlx_device) -> c_int;
        pub fn mlx_device_is_available(avail: *mut bool, dev: mlx_device) -> c_int;
        pub fn mlx_device_get_type(type_: *mut mlx_device_type, dev: mlx_device) -> c_int;

        pub fn mlx_stream_new_device(dev: mlx_device) -> mlx_stream;
        pub fn mlx_stream_free(stream: mlx_stream) -> c_int;
        pub fn mlx_synchronize(stream: mlx_stream) -> c_int;

        pub fn mlx_matmul(
            res: *mut mlx_array,
            a: mlx_array,
            b: mlx_array,
            stream: mlx_stream,
        ) -> c_int;
        pub fn mlx_add(
            res: *mut mlx_array,
            a: mlx_array,
            b: mlx_array,
            stream: mlx_stream,
        ) -> c_int;
        pub fn mlx_softmax_axis(
            res: *mut mlx_array,
            a: mlx_array,
            axis: c_int,
            precise: bool,
            stream: mlx_stream,
        ) -> c_int;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GatingSpec;
    use crate::training::{TrainingExample, TrainingSet};

    #[test]
    fn mlx_backend_predicts_dense_softmax_features() {
        let backend = MLXBackend::preferred().unwrap();
        assert!(matches!(backend.device_kind().unwrap(), "GPU" | "CPU"));

        let model = DeepModel {
            layers: vec![
                DeepLayerSpec::Input { dimensions: 2 },
                DeepLayerSpec::Dense {
                    weights: vec![2.0, 0.0, 0.0, 2.0],
                    bias: vec![0.0, 0.0],
                    output_channels: 2,
                    input_channels: 2,
                },
                DeepLayerSpec::Softmax,
            ],
            alpha: 0.0,
            gating: GatingSpec::None,
        };

        let (scores, probs) = backend
            .predict_features(&model, &[(7, vec![3.0, 0.0]), (9, vec![0.0, 3.0])])
            .unwrap();

        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|(_, score)| *score > 0.99), "{scores:?}");
        assert!(probs.get(&7).unwrap()[0] > 0.99);
        assert!(probs.get(&9).unwrap()[1] > 0.99);
    }

    #[test]
    fn mlx_backend_deep_learn_trains_and_predicts_features() {
        let backend = MLXBackend::preferred().unwrap();
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

        let output = backend
            .deep_learn(&training_set, &LearnOptions::default())
            .unwrap();
        assert_eq!(output.report.examples, 4);
        assert_eq!(output.report.feature_dimensions, 2);
        assert_eq!(output.report.class_count, 2);

        let (scores, probs) = backend
            .predict_features(&output.model, &[(7, vec![4.0, 0.0]), (9, vec![0.0, 4.0])])
            .unwrap();
        assert_eq!(scores.len(), 2);
        assert!(scores.iter().all(|(_, score)| *score > 0.99), "{scores:?}");
        assert!(probs.get(&7).unwrap()[0] > 0.99);
        assert!(probs.get(&9).unwrap()[1] > 0.99);
    }
}
