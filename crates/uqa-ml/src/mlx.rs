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

use crate::backend::{try_vec_with_capacity, CPUBackend, MLBackend, MLError, MLResult};
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
        let stream = RawStream::new(&device)?;
        Ok(Self { device, stream })
    }

    pub fn cpu() -> MLResult<Self> {
        let device = RawDevice::cpu()?;
        let stream = RawStream::new(&device)?;
        Ok(Self { device, stream })
    }

    pub fn device_kind(&self) -> MLResult<&'static str> {
        self.device.kind()
    }

    /// Release MLX stream and device handles while their C error codes can
    /// still be reported to the caller. `Drop` remains a best-effort fallback.
    pub fn close(mut self) -> MLResult<()> {
        let stream_result = self.stream.close();
        let device_result = self.device.close();
        match (stream_result, device_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(stream), Ok(())) => Err(stream),
            (Ok(()), Err(device)) => Err(device),
            (Err(stream), Err(device)) => Err(MLError::Backend(format!(
                "{stream}; additionally failed to release MLX device: {device}"
            ))),
        }
    }
}

impl MLBackend for MLXBackend {
    fn name(&self) -> &'static str {
        "mlx"
    }

    fn predict(&self, model: &DeepModel, ctx: &ExecutionContext) -> MLResult<PredictResult> {
        predict_cpu(model, ctx)
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
        let input_capacity = examples
            .len()
            .checked_mul(plan.input_channels)
            .ok_or_else(|| MLError::InvalidModel("MLX input size overflows usize".into()))?;
        let mut input = try_vec_with_capacity(input_capacity, "MLX input matrix")?;
        for (doc_id, features) in examples {
            if features.len() != plan.input_channels {
                return Err(MLError::InvalidModel(format!(
                    "feature vector for doc {doc_id} has dimension {}, expected {}",
                    features.len(),
                    plan.input_channels
                )));
            }
            for (index, value) in features.iter().enumerate() {
                input.push(f32_from_f64(
                    *value,
                    &format!("feature {index} for doc {doc_id}"),
                )?);
            }
        }

        let weight_count = plan
            .input_channels
            .checked_mul(plan.output_channels)
            .ok_or_else(|| MLError::InvalidModel("MLX weight count overflows usize".into()))?;
        let mut weights = try_vec_with_capacity(weight_count, "MLX weight matrix")?;
        for input_idx in 0..plan.input_channels {
            for output_idx in 0..plan.output_channels {
                let index = output_idx
                    .checked_mul(plan.input_channels)
                    .and_then(|index| index.checked_add(input_idx))
                    .ok_or_else(|| {
                        MLError::InvalidModel("MLX weight index overflows usize".into())
                    })?;
                weights.push(f32_from_f64(plan.weights[index], "dense weight")?);
            }
        }
        let mut bias = try_vec_with_capacity(plan.bias.len(), "MLX dense bias")?;
        for value in plan.bias {
            bias.push(f32_from_f64(*value, "dense bias")?);
        }

        let mut input = RawArray::from_f32(&[examples.len(), plan.input_channels], &input)?;
        let mut weights =
            RawArray::from_f32(&[plan.input_channels, plan.output_channels], &weights)?;
        let mut bias = RawArray::from_f32(&[plan.output_channels], &bias)?;

        let mut matrix_product = input.matmul(&weights, &self.stream)?;
        let mut logits = matrix_product.add(&bias, &self.stream)?;
        let mut probs = logits.softmax_axis(1, &self.stream)?;
        let flat_result = probs.to_f32_vec(&self.stream);
        let cleanup_results = [
            probs.close(),
            logits.close(),
            matrix_product.close(),
            bias.close(),
            weights.close(),
            input.close(),
        ];
        let flat = flat_result?;
        for cleanup in cleanup_results {
            cleanup?;
        }
        let expected_output = examples
            .len()
            .checked_mul(plan.output_channels)
            .ok_or_else(|| MLError::Backend("MLX output size overflows usize".into()))?;
        if flat.len() != expected_output {
            return Err(MLError::Backend(format!(
                "MLX returned {} probabilities, expected {expected_output}",
                flat.len()
            )));
        }

        let mut scores = try_vec_with_capacity(examples.len(), "MLX prediction scores")?;
        let mut class_probs = std::collections::BTreeMap::new();
        for (row, (doc_id, _)) in examples.iter().enumerate() {
            let offset = row
                .checked_mul(plan.output_channels)
                .ok_or_else(|| MLError::Backend("MLX output offset overflows usize".into()))?;
            let end = offset
                .checked_add(plan.output_channels)
                .ok_or_else(|| MLError::Backend("MLX output range overflows usize".into()))?;
            let mut row_probs =
                try_vec_with_capacity(plan.output_channels, "MLX class probabilities")?;
            row_probs.extend(flat[offset..end].iter().map(|value| f64::from(*value)));
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
            || output_channels
                .checked_mul(*input_channels)
                .is_none_or(|expected| weights.len() != expected)
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
    released: bool,
}

impl RawDevice {
    fn preferred() -> MLResult<Self> {
        let raw = unsafe { raw::mlx_device_new_type(raw::MLX_GPU, 0) };
        if raw.ctx.is_null() {
            return Self::cpu();
        }
        let mut gpu = Self {
            raw,
            released: false,
        };
        match gpu.is_available() {
            Ok(true) => return Ok(gpu),
            Ok(false) => gpu.close()?,
            Err(error) => {
                return match gpu.close() {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(MLError::Backend(format!(
                        "{error}; additionally failed to release unavailable MLX GPU: {cleanup}"
                    ))),
                };
            }
        }
        Self::cpu()
    }

    fn cpu() -> MLResult<Self> {
        let device = Self {
            raw: unsafe { raw::mlx_device_new_type(raw::MLX_CPU, 0) },
            released: false,
        };
        if device.raw.ctx.is_null() {
            Err(MLError::Backend(
                "mlx_device_new_type returned a null CPU device".into(),
            ))
        } else {
            Ok(device)
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

    fn close(&mut self) -> MLResult<()> {
        if self.released {
            return Ok(());
        }
        let code = unsafe { raw::mlx_device_free(self.raw) };
        self.released = true;
        check(code, "mlx_device_free")
    }
}

impl Drop for RawDevice {
    fn drop(&mut self) {
        if !self.released {
            unsafe {
                raw::mlx_device_free(self.raw);
            }
            self.released = true;
        }
    }
}

#[derive(Debug)]
struct RawStream {
    raw: raw::mlx_stream,
    released: bool,
}

impl RawStream {
    fn new(device: &RawDevice) -> MLResult<Self> {
        let stream = Self {
            raw: unsafe { raw::mlx_stream_new_device(device.raw) },
            released: false,
        };
        if stream.raw.ctx.is_null() {
            Err(MLError::Backend(
                "mlx_stream_new_device returned a null stream".into(),
            ))
        } else {
            Ok(stream)
        }
    }

    fn synchronize(&self) -> MLResult<()> {
        unsafe { check(raw::mlx_synchronize(self.raw), "mlx_synchronize") }
    }

    fn close(&mut self) -> MLResult<()> {
        if self.released {
            return Ok(());
        }
        let code = unsafe { raw::mlx_stream_free(self.raw) };
        self.released = true;
        check(code, "mlx_stream_free")
    }
}

impl Drop for RawStream {
    fn drop(&mut self) {
        if !self.released {
            unsafe {
                raw::mlx_stream_free(self.raw);
            }
            self.released = true;
        }
    }
}

#[derive(Debug)]
struct RawArray {
    raw: raw::mlx_array,
    released: bool,
}

impl RawArray {
    fn empty() -> raw::mlx_array {
        raw::mlx_array {
            ctx: ptr::null_mut(),
        }
    }

    fn from_f32(shape: &[usize], values: &[f32]) -> MLResult<Self> {
        let expected = shape.iter().try_fold(1usize, |product, dimension| {
            product
                .checked_mul(*dimension)
                .ok_or_else(|| MLError::Backend("MLX array shape overflows usize".into()))
        })?;
        if expected != values.len() {
            return Err(MLError::Backend(format!(
                "MLX array shape {shape:?} does not match {} values",
                values.len()
            )));
        }
        let mut converted_shape = try_vec_with_capacity(shape.len(), "MLX array shape")?;
        for dimension in shape {
            converted_shape.push(c_int::try_from(*dimension).map_err(|_| {
                MLError::Backend(format!("MLX array dimension {dimension} exceeds c_int"))
            })?);
        }
        let rank = c_int::try_from(converted_shape.len())
            .map_err(|_| MLError::Backend("MLX array rank exceeds c_int".into()))?;
        let raw = unsafe {
            raw::mlx_array_new_data(
                values.as_ptr().cast(),
                converted_shape.as_ptr(),
                rank,
                raw::MLX_FLOAT32,
            )
        };
        if raw.ctx.is_null() {
            return Err(MLError::Backend(
                "mlx_array_new_data returned a null handle".into(),
            ));
        }
        Ok(Self {
            raw,
            released: false,
        })
    }

    fn matmul(&self, rhs: &Self, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        let code =
            unsafe { raw::mlx_matmul(ptr::addr_of_mut!(out), self.raw, rhs.raw, stream.raw) };
        checked_array_result(out, code, "mlx_matmul")
    }

    fn add(&self, rhs: &Self, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        let code = unsafe { raw::mlx_add(ptr::addr_of_mut!(out), self.raw, rhs.raw, stream.raw) };
        checked_array_result(out, code, "mlx_add")
    }

    fn softmax_axis(&self, axis: i32, stream: &RawStream) -> MLResult<Self> {
        let mut out = Self::empty();
        let code = unsafe {
            raw::mlx_softmax_axis(ptr::addr_of_mut!(out), self.raw, axis, true, stream.raw)
        };
        checked_array_result(out, code, "mlx_softmax_axis")
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
            let mut values = try_vec_with_capacity(count, "MLX array output")?;
            values.extend_from_slice(slice::from_raw_parts(ptr, count));
            Ok(values)
        }
    }

    fn close(&mut self) -> MLResult<()> {
        if self.released {
            return Ok(());
        }
        let code = unsafe { raw::mlx_array_free(self.raw) };
        self.released = true;
        check(code, "mlx_array_free")
    }
}

impl Drop for RawArray {
    fn drop(&mut self) {
        if !self.released {
            unsafe {
                raw::mlx_array_free(self.raw);
            }
            self.released = true;
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

fn checked_array_result(
    raw_array: raw::mlx_array,
    code: c_int,
    context: &str,
) -> MLResult<RawArray> {
    if code != 0 {
        let cleanup_code = if raw_array.ctx.is_null() {
            0
        } else {
            unsafe { raw::mlx_array_free(raw_array) }
        };
        let cleanup = if cleanup_code == 0 {
            String::new()
        } else {
            format!("; mlx_array_free also failed with code {cleanup_code}")
        };
        return Err(MLError::Backend(format!(
            "{context} failed with MLX error code {code}{cleanup}"
        )));
    }
    if raw_array.ctx.is_null() {
        return Err(MLError::Backend(format!("{context} returned a null array")));
    }
    Ok(RawArray {
        raw: raw_array,
        released: false,
    })
}

fn f32_from_f64(value: f64, context: &str) -> MLResult<f32> {
    if !value.is_finite() {
        return Err(MLError::InvalidModel(format!(
            "{context} must be finite, got {value}"
        )));
    }
    if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(MLError::InvalidModel(format!(
            "{context} is outside the f32 range: {value}"
        )));
    }
    Ok(value as f32)
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
        backend.close().unwrap();
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
        backend.close().unwrap();
    }
}
