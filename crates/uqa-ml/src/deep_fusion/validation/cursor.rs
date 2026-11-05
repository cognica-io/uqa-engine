//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shape admission and finite-value windows share ordinary and suspended model validation.

use super::{validate_layer, Layer, MLError, MLResult};
use std::fmt::{Display, Formatter};

pub(super) enum Inputs<'a> {
    Runtime(std::slice::Iter<'a, crate::Layer>),
    Model(std::slice::Iter<'a, crate::DeepLayerSpec>),
}

impl<'a> Iterator for Inputs<'a> {
    type Item = Layer<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Runtime(layers) => layers.next().map(Layer::from),
            Self::Model(layers) => layers.next().map(Layer::from),
        }
    }
}

pub(super) struct LayerContext(pub usize);

impl Display for LayerContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "deep-fusion layer {}", self.0)
    }
}

pub(super) struct Numbers<'a> {
    fields: [(&'a [f64], &'static str); 3],
    count: usize,
    field: usize,
    offset: usize,
    pub(super) hops: bool,
    sum: f64,
    negative: bool,
}

impl<'a> Numbers<'a> {
    fn new() -> Self {
        Self {
            fields: [(&[], ""); 3],
            count: 0,
            field: 0,
            offset: 0,
            hops: false,
            sum: -0.0,
            negative: false,
        }
    }

    pub(super) fn push(&mut self, values: &'a [f64], name: &'static str) {
        self.fields[self.count] = (values, name);
        self.count += 1;
    }

    fn step(&mut self, index: usize) -> MLResult<bool> {
        let context = LayerContext(index);
        let (values, name) = self.fields[self.field];
        let end = self.offset.saturating_add(512).min(values.len());
        for (offset, &value) in values[self.offset..end].iter().enumerate() {
            if !value.is_finite() {
                let offset = self.offset + offset;
                return Err(MLError::InvalidModel(format!(
                    "{context}: {name}[{offset}] must be finite, got {value}"
                )));
            }
            if self.hops {
                self.sum += value;
                self.negative |= value < 0.0;
            }
        }
        self.offset = end;
        if self.offset == values.len() {
            self.field += 1;
            self.offset = 0;
        }
        let complete = self.field == self.count;
        if complete && self.hops && (self.negative || !self.sum.is_finite() || self.sum <= 0.0) {
            return Err(MLError::InvalidModel(format!(
                "{context}: hop_weights must be non-negative with a finite positive sum"
            )));
        }
        Ok(complete)
    }
}

pub(crate) struct ModelValidation<'a> {
    layers: Inputs<'a>,
    first: Option<Layer<'a>>,
    index: usize,
    numbers: Option<Numbers<'a>>,
    complete: bool,
}

impl<'a> ModelValidation<'a> {
    pub(crate) fn model(layers: &'a [crate::DeepLayerSpec], alpha: f64) -> MLResult<Self> {
        Self::new(Inputs::Model(layers.iter()), alpha)
    }

    pub(super) fn new(mut layers: Inputs<'a>, alpha: f64) -> MLResult<Self> {
        let Some(first) = layers.next() else {
            return Err(MLError::InvalidModel(
                "DeepFusionOperator requires at least one layer".into(),
            ));
        };
        if !matches!(
            first,
            Layer::Signal(_) | Layer::Embed(_) | Layer::Input { .. }
        ) {
            return Err(MLError::InvalidModel(
                "the first deep-fusion layer must be Signal, Embed, or Input".into(),
            ));
        }
        if !alpha.is_finite() {
            return Err(MLError::InvalidModel(format!(
                "deep-fusion alpha must be finite, got {alpha}"
            )));
        }
        Ok(Self {
            layers,
            first: Some(first),
            index: 0,
            numbers: None,
            complete: false,
        })
    }

    /// One position checks one layer's fixed metadata or at most 512 numeric values.
    pub(crate) fn advance(&mut self) -> MLResult<bool> {
        if self.complete {
            return Ok(true);
        }
        if let Some(numbers) = &mut self.numbers {
            if numbers.step(self.index - 1)? {
                self.numbers = None;
            }
        } else if let Some(layer) = self.first.take().or_else(|| self.layers.next()) {
            let mut numbers = Numbers::new();
            validate_layer(self.index, &layer, &mut numbers)?;
            self.index += 1;
            if numbers.count != 0 {
                self.numbers = Some(numbers);
            }
        } else {
            self.complete = true;
        }
        Ok(self.complete)
    }

    pub(super) fn finish(mut self) -> MLResult<()> {
        while !self.advance()? {}
        Ok(())
    }
}
