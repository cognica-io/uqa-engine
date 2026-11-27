//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Request-owned value metadata for captured partition comparison; no runtime cache is populated.

use crate::catalog::CatalogReadView;
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_core::memory::{BudgetedMap, Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    expr::{
        enums::{EnumTypeLabel, EnumTypeLabels},
        RangeValueDescriptor, SQLValueCatalog,
    },
    SQLError,
};

/// Arc-returning value interfaces are borrowed only during native validation. These private maps retain each admitted payload until the complete comparison finishes; no returned Arc escapes the validation call.
pub(super) struct PartitionValues<'a> {
    catalog: &'a CatalogReadView,
    metadata: Option<&'a dyn uqa_sql::routines::RoutineResolution>,
    enums: Mutex<BudgetedMap<u32, Produced<Arc<EnumTypeLabels>>>>,
    ranges: Mutex<BudgetedMap<u32, Produced<Arc<RangeValueDescriptor>>>>,
    control: ProductionControl<'a>,
}

impl<'a> PartitionValues<'a> {
    pub fn new(
        catalog: &'a CatalogReadView,
        metadata: Option<&'a dyn uqa_sql::routines::RoutineResolution>,
        control: ProductionControl<'a>,
    ) -> Self {
        let memory = control
            .budget()
            .expect("native catalog validation allowance");
        Self {
            catalog,
            metadata,
            enums: Mutex::new(BudgetedMap::new(memory)),
            ranges: Mutex::new(BudgetedMap::new(memory)),
            control,
        }
    }

    fn shared<T>(&self, value: Produced<T>) -> Result<Produced<Arc<T>>, SQLError> {
        let header = self.control.reserve(std::mem::size_of::<T>())?;
        let (value, memory) = value.into_parts();
        Ok(self
            .control
            .finish(Arc::new(value), self.control.combine(header, memory))?)
    }
}

impl SQLValueCatalog for PartitionValues<'_> {
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        self.control.check()?;
        if let Some(labels) = self.enums.lock().get(&oid) {
            return Ok(Some(Arc::clone(labels)));
        }
        for definition in self.catalog.enums() {
            self.control.check()?;
            if definition.oid != oid {
                continue;
            }
            let mut labels = ProductionVec::new(self.control);
            for label in &definition.labels {
                self.control.check()?;
                let key_memory = self.control.reserve(label.key.as_bytes().len())?;
                let key = self.control.finish(label.key.clone(), key_memory)?;
                let text = self.control.copy_text(&label.label)?;
                let (key, key_memory) = key.into_parts();
                let (label_text, text_memory) = text.into_parts();
                labels.push_produced(self.control.finish(
                    EnumTypeLabel {
                        oid: label.oid,
                        key,
                        label: label_text,
                    },
                    self.control.combine(key_memory, text_memory),
                )?)?;
            }
            let (labels, memory) = labels.finish()?.into_parts();
            let labels = self.shared(self.control.finish(
                EnumTypeLabels {
                    type_oid: oid,
                    labels,
                },
                memory,
            )?)?;
            let result = Arc::clone(&labels);
            self.enums.lock().insert(oid, labels)?;
            return Ok(Some(result));
        }
        Ok(None)
    }

    fn enum_label_position(&self, oid: u32) -> Result<Option<(u32, usize)>, SQLError> {
        for definition in self.catalog.enums() {
            self.control.check()?;
            for (position, label) in definition.labels.iter().enumerate() {
                self.control.check()?;
                if label.oid == oid {
                    return Ok(Some((definition.oid, position)));
                }
            }
        }
        Ok(None)
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        false
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        self.catalog.enums().next().is_some()
    }

    fn value_type_by_oid_with_control(
        &self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<uqa_sql::ColumnType>>, SQLError> {
        self.control.check()?;
        self.metadata
            .and_then(|routines| routines.enum_labels())
            .map(|metadata| metadata.value_type_by_oid_with_control(oid, control))
            .transpose()
            .map(Option::flatten)
    }

    fn value_composite_type_with_control(
        &self,
        oid: u32,
        control: &ProductionControl<'_>,
    ) -> Result<Option<uqa_sql::expr::composites::OwnedCompositeTypeDescriptor>, SQLError> {
        self.control.check()?;
        self.metadata
            .and_then(|routines| routines.enum_labels())
            .map(|metadata| metadata.value_composite_type_with_control(oid, control))
            .transpose()
            .map(Option::flatten)
    }

    fn compare_range_bounds(
        &self,
        oid: u32,
        left: &uqa_core::Value,
        right: &uqa_core::Value,
        control: &ProductionControl<'_>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        for definition in self.catalog.ranges() {
            self.control.check()?;
            if definition.oid == oid || definition.multirange.oid == oid {
                return crate::catalog::range_type::support::compare_definition_bounds(
                    definition,
                    self,
                    None,
                    (left, right),
                    control,
                );
            }
        }
        Err(SQLError::Internal(format!(
            "cache lookup failed for range type {oid}"
        )))
    }

    fn value_range_type(&self, oid: u32) -> Result<Option<Arc<RangeValueDescriptor>>, SQLError> {
        self.control.check()?;
        if let Some(descriptor) = self.ranges.lock().get(&oid) {
            return Ok(Some(Arc::clone(descriptor)));
        }
        for definition in self.catalog.ranges() {
            self.control.check()?;
            if definition.oid != oid && definition.multirange.oid != oid {
                continue;
            }
            let (endpoint, memory) = definition
                .subtype
                .clone_with_control(&self.control)?
                .into_parts();
            let descriptor = self.shared(self.control.finish(
                RangeValueDescriptor {
                    cast_oid: definition.cast_oid,
                    range_oid: definition.oid,
                    multirange_oid: definition.multirange.oid,
                    endpoint,
                },
                memory,
            )?)?;
            let result = Arc::clone(&descriptor);
            self.ranges.lock().insert(oid, descriptor)?;
            return Ok(Some(result));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
