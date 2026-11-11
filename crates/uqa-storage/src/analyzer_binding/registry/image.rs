//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encoded binding children retain both historical sides without executable owners.

use super::{Key, StoredKey};
use crate::{
    analyzer_binding::{
        capture::{CapturedBoundAnalyzerRevision, CapturedFieldAnalyzerBinding},
        AnalyzerBindingOwner, BoundAnalyzerRevision, FieldAnalyzerBinding,
    },
    analyzer_image::AnalyzerImageSnapshot,
    read_control::StorageReadControl,
    AnalyzerPhase, StorageBackendError, StorageBackendResult,
};
use parking_lot::Mutex;
use std::sync::Arc;
use uqa_analysis::AnalyzerImageSource;
use uqa_core::memory::{
    Budgeted, MemoryError, MemoryReservation, OwnedMap, Produced, ProductionControl, RetainedMap,
    RetainedMapSnapshot,
};

struct ImageSide {
    name: Option<String>,
    image: AnalyzerImageSource,
}

pub(super) struct ImageBinding {
    index: ImageSide,
    search: ImageSide,
    owner: AnalyzerBindingOwner,
    last_phase: AnalyzerPhase,
    pub(super) bytes: usize,
}

fn copy_name(
    name: Option<&str>,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<Option<String>>> {
    let (name, memory) = match name {
        Some(name) => {
            let (name, memory) = control.copy_text(name)?.into_parts();
            (Some(name), memory)
        }
        None => (None, control.empty_reservation()),
    };
    Ok(control.finish(name, memory)?)
}

impl ImageBinding {
    pub(super) fn prepare(
        value: &FieldAnalyzerBinding,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Arc<Produced<Self>>> {
        let side = |value: &BoundAnalyzerRevision| -> StorageBackendResult<_> {
            let name = copy_name(value.name.as_deref(), control)?;
            let image = value.compiled.image_source_with_control(control)?;
            Self::side(name, image, control)
        };
        Self::finish(
            side(&value.index)?,
            side(&value.search)?,
            value.owner,
            value.last_phase,
            control,
        )
    }

    fn side(
        name: Produced<Option<String>>,
        image: AnalyzerImageSource,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Produced<ImageSide>> {
        let (name, memory) = name.into_parts();
        Ok(control.finish(ImageSide { name, image }, memory)?)
    }

    fn finish(
        index: Produced<ImageSide>,
        search: Produced<ImageSide>,
        owner: AnalyzerBindingOwner,
        last_phase: AnalyzerPhase,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<Arc<Produced<Self>>> {
        let mut bytes = size_of::<Produced<Self>>();
        for side in [&*index, &*search] {
            bytes = bytes
                .checked_add(side.name.as_ref().map_or(0, String::capacity))
                .and_then(|bytes| bytes.checked_add(side.image.retained_bytes()))
                .ok_or(MemoryError::SizeOverflow)?;
        }
        let (index, index_memory) = index.into_parts();
        let (search, search_memory) = search.into_parts();
        Ok(control
            .finish(
                Self {
                    index,
                    search,
                    owner,
                    last_phase,
                    bytes,
                },
                control.combine(index_memory, search_memory),
            )?
            .into_shared(control)?)
    }

    pub(super) async fn prepare_owned(
        value: Budgeted<FieldAnalyzerBinding<uqa_analysis::CompiledAnalyzer>>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<(Budgeted<FieldAnalyzerBinding>, Arc<Produced<Self>>)> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        let result = value
            .try_map_async(|value| async move {
                let index_name = copy_name_from(value.index.name.as_deref(), input).await?;
                let (index_compiled, index_image) = value
                    .index
                    .compiled
                    .into_image_source_with_input(input)
                    .await?;
                let index = Self::side(index_name, index_image, &control)?;
                let search_name = copy_name_from(value.search.name.as_deref(), input).await?;
                let (search_compiled, search_image) = value
                    .search
                    .compiled
                    .into_image_source_with_input(input)
                    .await?;
                let search = Self::side(search_name, search_image, &control)?;
                let image = Self::finish(index, search, value.owner, value.last_phase, &control)?;
                let value = FieldAnalyzerBinding {
                    index: BoundAnalyzerRevision {
                        name: value.index.name,
                        compiled: index_compiled,
                    },
                    search: BoundAnalyzerRevision {
                        name: value.search.name,
                        compiled: search_compiled,
                    },
                    owner: value.owner,
                    last_phase: value.last_phase,
                };
                Ok::<_, StorageBackendError>((value, image))
            })
            .await?;
        let ((value, image), memory) = result.into_parts();
        Ok((Budgeted::new(value, memory), image))
    }
}

async fn copy_name_from(
    name: Option<&str>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> StorageBackendResult<Produced<Option<String>>> {
    let (name, memory) = match name {
        Some(name) => {
            let (name, memory) = input.copy_text(name).await?.into_parts();
            (Some(name), memory)
        }
        None => (None, input.memory.empty_reservation()),
    };
    Ok(Budgeted::new(name, memory).into())
}

type Child = Arc<Budgeted<CapturedFieldAnalyzerBinding>>;
struct State {
    children: OwnedMap<Key, Budgeted<Child>>,
    remaining: MemoryReservation,
}

pub struct CapturedFieldAnalyzers {
    values: RetainedMapSnapshot<StoredKey, Arc<Produced<ImageBinding>>>,
    state: Mutex<State>,
    control: StorageReadControl,
}

impl CapturedFieldAnalyzers {
    /// Seek the next retained identity in logarithmic time, without allocating or visiting earlier entries.
    pub fn next_key(&self, after: Option<&Key>) -> StorageBackendResult<Option<&Key>> {
        self.control.check()?;
        Ok(self
            .values
            .first_from(after.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded))
            .map(|(name, _)| &*name.0))
    }

    pub(super) fn capture(
        values: &RetainedMap<StoredKey, Arc<Produced<ImageBinding>>>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Self>> {
        control.check()?;
        let (values, remaining) = values.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(
            Self {
                values,
                state: Mutex::new(State {
                    children: OwnedMap::new(),
                    remaining,
                }),
                control: control.clone(),
            },
            control.memory().empty_reservation(),
        ))
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
    pub fn keys(&self) -> impl Iterator<Item = &Key> {
        self.values.iter().map(|(key, _)| &*key.0)
    }

    pub fn binding(&self, key: &Key) -> StorageBackendResult<Option<Child>> {
        self.control.check()?;
        let Some(value) = self.values.get(key) else {
            return Ok(None);
        };
        let mut state = self.state.lock();
        self.control.check()?;
        if let Some(child) = state.children.get(key) {
            return Ok(Some((**child).clone()));
        }
        let control = ProductionControl::new(
            self.control.memory(),
            self.control.cancellation(),
            self.control.cancellation(),
        );
        let (cache_key, key_memory) = StoredKey::copy(key, &control)?.0.into_parts();
        let (index_name, index_memory) =
            copy_name(value.index.name.as_deref(), &control)?.into_parts();
        let (search_name, search_memory) =
            copy_name(value.search.name.as_deref(), &control)?.into_parts();
        let names_memory = control.combine(index_memory, search_memory);
        let mut binding_memory = self
            .control
            .memory()
            .reserve(size_of::<Budgeted<CapturedFieldAnalyzerBinding>>())?;
        binding_memory.absorb(names_memory.expect("binding names have their capture allowance"));
        let mut index_memory = self
            .control
            .memory()
            .reserve(AnalyzerImageSnapshot::wrapper_bytes())?;
        let mut search_memory = self
            .control
            .memory()
            .reserve(AnalyzerImageSnapshot::wrapper_bytes())?;
        let State {
            children,
            remaining,
        } = &mut *state;
        children.insert_budgeted_with(
            cache_key,
            self.control.memory(),
            || -> StorageBackendResult<_> {
                self.control.check()?;
                index_memory.absorb(remaining.split(value.index.image.retained_bytes()));
                search_memory.absorb(remaining.split(value.search.image.retained_bytes()));
                let index = CapturedBoundAnalyzerRevision {
                    name: index_name,
                    image: AnalyzerImageSnapshot::from_admitted(
                        value.index.image.clone(),
                        &self.control,
                        index_memory,
                    ),
                };
                let search = CapturedBoundAnalyzerRevision {
                    name: search_name,
                    image: AnalyzerImageSnapshot::from_admitted(
                        value.search.image.clone(),
                        &self.control,
                        search_memory,
                    ),
                };
                Ok(Budgeted::new(
                    Arc::new(Budgeted::new(
                        CapturedFieldAnalyzerBinding {
                            index,
                            search,
                            owner: value.owner,
                            last_phase: value.last_phase,
                        },
                        binding_memory,
                    )),
                    key_memory.expect("binding cache key has its capture allowance"),
                ))
            },
        )?;
        Ok(Some(
            (**children
                .get(key)
                .expect("selected field binding was cached"))
            .clone(),
        ))
    }
}
