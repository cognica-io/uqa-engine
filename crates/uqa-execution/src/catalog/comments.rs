//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Transactional descriptions keyed by catalog object address, independently of its current name or type form.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    sync::Arc,
};
use uqa_sql::{ast::RelationPersistence, catalog::dependencies::ObjectAddress, SQLError};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

/// Object-first ordering gives both forms of `obj_description` a bounded key lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommentAddress {
    object_id: u32,
    sub_id: i32,
    class_id: u32,
}
impl From<ObjectAddress> for CommentAddress {
    fn from(object: ObjectAddress) -> Self {
        Self {
            object_id: object.object_id,
            sub_id: object.sub_id,
            class_id: object.class_id,
        }
    }
}
mod registry;
pub use registry::{CapturedComments, CommentRegistry};
pub mod capture;
pub mod snapshot;
pub trait CommentPublication {
    fn comments(&self) -> Box<dyn Deref<Target = CommentRegistry> + '_>;
    fn comment_catalog(&self) -> Option<&dyn CatalogFacade>;
    fn publish_comments(&self, registry: CommentRegistry);
}

const PREFIX: &str = "uqa.sql.comment.v1:";

fn key(address: CommentAddress) -> String {
    format!(
        "{PREFIX}{}:{}:{}",
        address.class_id, address.object_id, address.sub_id
    )
}

fn address(key_text: &str) -> Option<CommentAddress> {
    let mut parts = key_text.strip_prefix(PREFIX)?.split(':');
    let value = CommentAddress {
        class_id: parts.next()?.parse().ok()?,
        object_id: parts.next()?.parse().ok()?,
        sub_id: parts.next()?.parse().ok()?,
    };
    (parts.next().is_none()
        && value.class_id != 0
        && value.object_id != 0
        && value.sub_id >= 0
        && key(value) == key_text)
        .then_some(value)
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    current: &CommentRegistry,
) -> StorageBackendResult<CommentRegistry> {
    let descriptions = catalog
        .metadata_with_prefix(PREFIX)?
        .into_iter()
        .map(|(name, description)| {
            let object = address(&name).ok_or_else(|| {
                StorageBackendError::Other("invalid catalog comment address".into())
            })?;
            if description.is_empty() || description.contains('\0') {
                return Err(StorageBackendError::Other(
                    "invalid catalog comment text".into(),
                ));
            }
            Ok((object, description))
        })
        .collect::<StorageBackendResult<BTreeMap<_, _>>>()?;
    let mut registry = current.durable_generation();
    for (object, description) in &descriptions {
        if registry.durable_get(object) != Some(description) {
            registry
                .insert(*object, description, RelationPersistence::Permanent)
                .map_err(|error| StorageBackendError::backend("comment restoration", error))?;
        }
    }
    for (object, _) in current.durable_iter() {
        if !descriptions.contains_key(object) {
            registry
                .remove(object)
                .map_err(|error| StorageBackendError::backend("comment restoration", error))?;
        }
    }
    Ok(registry.with_temporary_from(current))
}

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &CommentRegistry,
    committed: Arc<CommentRegistry>,
) -> StorageBackendResult<Arc<CommentRegistry>> {
    let mut committed = Arc::new(committed.with_temporary_from(current));
    let addresses = current
        .durable_iter()
        .chain(committed.durable_iter())
        .map(|(address, _)| address)
        .copied()
        .collect::<BTreeSet<_>>();
    for object in addresses {
        if catalog.map_or(Ok(false), |catalog| {
            catalog.metadata_has_private_changes(&key(object))
        })? {
            if let Some(description) = current.durable_get(&object) {
                Arc::make_mut(&mut committed)
                    .insert(object, description, RelationPersistence::Permanent)
                    .map_err(|error| {
                        StorageBackendError::backend("private comment merge", error)
                    })?;
            } else {
                Arc::make_mut(&mut committed)
                    .remove(&object)
                    .map_err(|error| {
                        StorageBackendError::backend("private comment merge", error)
                    })?;
            }
        }
    }
    Ok(committed)
}

/// The caller retains the addressed object's lock and existing definition transaction.
pub fn set(
    publication: &dyn CommentPublication,
    object: ObjectAddress,
    persistence: RelationPersistence,
    description: Option<&str>,
) -> Result<(), SQLError> {
    let object = CommentAddress::from(object);
    let description = description.filter(|text| !text.is_empty());
    let mut registry = publication.comments().clone();
    let was_durable = registry.get(&object).is_some() && !registry.is_temporary(&object);
    if let Some(text) = description {
        registry.insert(object, text, persistence)?;
    } else {
        registry.remove(&object)?;
    }
    if let Some(catalog) = publication.comment_catalog() {
        match (persistence, description) {
            (RelationPersistence::Temporary, _) if !was_durable => Ok(()),
            (RelationPersistence::Temporary, _) | (_, None) => {
                catalog.delete_metadata(&key(object))
            }
            (_, Some(text)) => catalog.set_metadata(&key(object), text),
        }
        .map_err(|error| uqa_sql::catalog::errors::storage_error("catalog comment", &error))?;
    }
    publication.publish_comments(registry);
    Ok(())
}

/// Drop descriptions in the same transaction that removes their owning objects.
pub fn remove(
    publication: &dyn CommentPublication,
    class: u32,
    targets: &BTreeSet<u32>,
) -> Result<bool, SQLError> {
    let comments = publication.comments();
    let addresses = targets
        .iter()
        .flat_map(|&object_id| {
            let first = CommentAddress {
                object_id,
                sub_id: 0,
                class_id: 0,
            };
            let last = CommentAddress {
                object_id,
                sub_id: i32::MAX,
                class_id: u32::MAX,
            };
            comments
                .range(first..=last)
                .filter_map(move |(object, _)| (object.class_id == class).then_some(*object))
        })
        .map(|object| (object, comments.is_temporary(&object)))
        .collect::<Vec<_>>();
    drop(comments);
    if addresses.is_empty() {
        return Ok(false);
    }
    let mut registry = publication.comments().clone();
    for (object, _) in &addresses {
        registry.remove(object)?;
    }
    for (object, temporary) in addresses {
        if temporary {
            continue;
        }
        if let Some(catalog) = publication.comment_catalog() {
            catalog.delete_metadata(&key(object)).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("remove catalog comment", &error)
            })?;
        }
    }
    publication.publish_comments(registry);
    Ok(true)
}

pub fn rows(catalog: &super::CatalogReadView) -> Vec<uqa_sql::ResultRow> {
    use uqa_core::Value;
    catalog
        .snapshot()
        .definitions
        .comments
        .iter()
        .map(|(object, description)| {
            BTreeMap::from([
                ("objoid".into(), Value::Int(i64::from(object.object_id))),
                ("classoid".into(), Value::Int(i64::from(object.class_id))),
                ("objsubid".into(), Value::Int(i64::from(object.sub_id))),
                ("description".into(), Value::Str(description.clone())),
            ])
        })
        .collect()
}

pub fn object_description(
    catalog: &super::CatalogReadView,
    arguments: &[uqa_core::Value],
) -> Result<uqa_core::Value, SQLError> {
    use uqa_core::Value;
    if arguments.contains(&Value::Null) {
        return Ok(Value::Null);
    }
    let (oid, class) = match arguments {
        [Value::Int(oid)] => (*oid, None),
        [Value::Int(oid), Value::Str(class)] => {
            let Some(class) = uqa_sql::catalog::SystemRelation::at("pg_catalog", class) else {
                return Ok(Value::Null);
            };
            (*oid, Some(class.oid()))
        }
        _ => {
            return Err(SQLError::TypeMismatch(
                "obj_description requires an oid and an optional catalog name".into(),
            ))
        }
    };
    let Ok(object_id) = u32::try_from(oid) else {
        return Ok(Value::Null);
    };
    let snapshot = catalog.snapshot();
    let comments = &snapshot.definitions.comments;
    let first = CommentAddress {
        object_id,
        sub_id: 0,
        class_id: 0,
    };
    let description = if let Some(class_id) = class {
        u32::try_from(class_id)
            .ok()
            .and_then(|class_id| comments.get(&CommentAddress { class_id, ..first }))
    } else {
        let last = CommentAddress {
            class_id: u32::MAX,
            ..first
        };
        comments.range(first..=last).next().map(|(_, text)| text)
    };
    Ok(description.map_or(Value::Null, |text| Value::Str(text.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

    struct Publication {
        catalog: KeyValueCatalog,
        comments: RefCell<CommentRegistry>,
        persistence_calls: Cell<usize>,
    }
    impl CommentPublication for Publication {
        fn comments(&self) -> Box<dyn Deref<Target = CommentRegistry> + '_> {
            Box::new(self.comments.borrow())
        }
        fn comment_catalog(&self) -> Option<&dyn CatalogFacade> {
            self.persistence_calls.set(self.persistence_calls.get() + 1);
            Some(&self.catalog)
        }
        fn publish_comments(&self, registry: CommentRegistry) {
            *self.comments.borrow_mut() = registry;
        }
    }

    #[test]
    fn temporary_descriptions_survive_refresh_without_entering_durable_metadata() {
        let publication = Publication {
            catalog: KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
            comments: RefCell::default(),
            persistence_calls: Cell::new(0),
        };
        let durable = ObjectAddress::whole(1247, 20_000);
        let temporary = ObjectAddress::whole(1247, 20_001);
        set(
            &publication,
            durable,
            RelationPersistence::Unlogged,
            Some("durable"),
        )
        .unwrap();
        set(
            &publication,
            temporary,
            RelationPersistence::Temporary,
            Some("temporary"),
        )
        .unwrap();
        let current = publication.comments.borrow().clone();
        let restored = restore(&publication.catalog, &current).unwrap();
        assert_eq!(restored, current);
        assert!(std::ptr::eq(
            restored.get(&temporary.into()).unwrap(),
            current.get(&temporary.into()).unwrap()
        ));
        let fresh = restore(&publication.catalog, &CommentRegistry::new()).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[&durable.into()], "durable");
        let merged = merge_private(None, &current, Arc::new(fresh)).unwrap();
        assert_eq!(*merged, current);
        set(
            &publication,
            durable,
            RelationPersistence::Temporary,
            Some("now temporary"),
        )
        .unwrap();
        assert!(publication
            .catalog
            .metadata_with_prefix(PREFIX)
            .unwrap()
            .is_empty());
        assert_eq!(publication.comments.borrow().len(), 2);
        set(
            &publication,
            temporary,
            RelationPersistence::Permanent,
            Some("now durable"),
        )
        .unwrap();
        let fresh = restore(&publication.catalog, &CommentRegistry::new()).unwrap();
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[&temporary.into()], "now durable");
        remove(
            &publication,
            1247,
            &BTreeSet::from([durable.object_id, temporary.object_id]),
        )
        .unwrap();
        assert!(publication.comments.borrow().is_empty());
        assert!(publication
            .catalog
            .metadata_with_prefix(PREFIX)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn object_descriptions_restore_replace_and_drop_by_identity() {
        let publication = Publication {
            catalog: KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
            comments: RefCell::default(),
            persistence_calls: Cell::new(0),
        };
        let first = ObjectAddress::whole(uqa_sql::catalog::dependencies::TYPE_CLASS, 20_000);
        let second = ObjectAddress::whole(uqa_sql::catalog::dependencies::TYPE_CLASS, 20_001);
        set(
            &publication,
            first,
            RelationPersistence::Permanent,
            Some("original\n설명"),
        )
        .unwrap();
        set(
            &publication,
            second,
            RelationPersistence::Permanent,
            Some("independent"),
        )
        .unwrap();
        assert_eq!(
            restore(&publication.catalog, &publication.comments.borrow()).unwrap(),
            *publication.comments.borrow()
        );
        set(
            &publication,
            first,
            RelationPersistence::Permanent,
            Some("replacement"),
        )
        .unwrap();
        assert_eq!(
            restore(&publication.catalog, &publication.comments.borrow()).unwrap()[&first.into()],
            "replacement"
        );
        remove(
            &publication,
            first.class_id,
            &BTreeSet::from([first.object_id]),
        )
        .unwrap();
        assert_eq!(
            restore(&publication.catalog, &publication.comments.borrow()).unwrap(),
            CommentRegistry::try_from_iter([(
                second.into(),
                "independent".into(),
                RelationPersistence::Permanent
            )])
            .unwrap()
        );
        set(
            &publication,
            second,
            RelationPersistence::Permanent,
            Some(""),
        )
        .unwrap();
        assert!(
            restore(&publication.catalog, &publication.comments.borrow())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn malformed_description_addresses_cannot_alias_a_catalog_object() {
        for (name, text) in [
            ("0:20000:0", "description"),
            ("1247:0:0", "description"),
            ("1247:20000:-1", "description"),
            ("1247:020000:0", "description"),
            ("1247:20000:0:extra", "description"),
            ("1247:20000:0", ""),
            ("1247:20000:0", "invalid\0text"),
        ] {
            let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
            catalog
                .set_metadata(&format!("{PREFIX}{name}"), text)
                .unwrap();
            assert!(
                restore(
                    &catalog,
                    &CommentRegistry::with_budget(&uqa_core::memory::MemoryBudget::new(4096))
                )
                .is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn native_comment_admission_finishes_before_any_persistence_or_publication() {
        let memory = uqa_core::memory::MemoryBudget::new(1 << 20);
        let publication = Publication {
            catalog: KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
            comments: RefCell::new(CommentRegistry::with_budget(&memory)),
            persistence_calls: Cell::new(0),
        };
        for oid in [20_000, 20_001, 20_002] {
            set(
                &publication,
                ObjectAddress::whole(1247, oid),
                RelationPersistence::Permanent,
                Some("original"),
            )
            .unwrap();
        }
        let original = publication.comments.borrow().clone();
        let before = publication.catalog.metadata_with_prefix(PREFIX).unwrap();
        publication.persistence_calls.set(0);
        let occupied = memory.reserve(memory.available()).unwrap();
        assert_eq!(
            set(
                &publication,
                ObjectAddress::whole(1247, 20_000),
                RelationPersistence::Permanent,
                Some("replacement")
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
        assert_eq!(
            remove(&publication, 1247, &BTreeSet::from([20_000]))
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
        assert_eq!(publication.persistence_calls.get(), 0);
        assert_eq!(*publication.comments.borrow(), original);
        assert_eq!(
            publication.catalog.metadata_with_prefix(PREFIX).unwrap(),
            before
        );
        drop(occupied);
        set(
            &publication,
            ObjectAddress::whole(1247, 20_000),
            RelationPersistence::Permanent,
            Some("replacement"),
        )
        .unwrap();
        assert_eq!(
            publication.comments.borrow()[&ObjectAddress::whole(1247, 20_000).into()],
            "replacement"
        );
        drop((publication, original));
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn native_comment_restoration_releases_failed_admission_without_changing_metadata() {
        let catalog = KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new()));
        for oid in [20_000, 20_001, 20_002] {
            catalog
                .set_metadata(
                    &key(ObjectAddress::whole(1247, oid).into()),
                    "original 한글",
                )
                .unwrap();
        }
        let expected = catalog.metadata_with_prefix(PREFIX).unwrap();
        let exact = uqa_core::memory::MemoryBudget::new(1 << 20);
        let loaded = restore(&catalog, &CommentRegistry::with_budget(&exact)).unwrap();
        let required = exact.used();
        drop(loaded);
        assert_eq!(exact.used(), 0);
        let denied = uqa_core::memory::MemoryBudget::new(required - 1);
        assert!(restore(&catalog, &CommentRegistry::with_budget(&denied)).is_err());
        assert_eq!(denied.used(), 0);
        assert_eq!(catalog.metadata_with_prefix(PREFIX).unwrap(), expected);
        let loaded = restore(&catalog, &CommentRegistry::with_budget(&exact)).unwrap();
        assert_eq!(loaded.len(), 3);
        let occupied = exact.reserve(exact.available()).unwrap();
        let unchanged = restore(&catalog, &loaded).unwrap();
        for (object, text) in loaded.iter() {
            assert!(std::ptr::eq(unchanged.get(object).unwrap(), text));
        }
        drop((occupied, unchanged));
        let removed = CommentAddress::from(ObjectAddress::whole(1247, 20_000));
        let replaced = CommentAddress::from(ObjectAddress::whole(1247, 20_001));
        let retained = CommentAddress::from(ObjectAddress::whole(1247, 20_002));
        let added = CommentAddress::from(ObjectAddress::whole(1247, 20_003));
        catalog.delete_metadata(&key(removed)).unwrap();
        catalog.set_metadata(&key(replaced), "replacement").unwrap();
        catalog
            .set_metadata(&key(added), "new description")
            .unwrap();
        let changed = catalog.metadata_with_prefix(PREFIX).unwrap();
        let occupied = exact.reserve(exact.available()).unwrap();
        let error = restore(&catalog, &loaded).unwrap_err();
        assert_eq!(
            uqa_sql::catalog::errors::storage_error("comment restoration", &error).sqlstate(),
            Some("53200")
        );
        assert_eq!(loaded.len(), 3);
        assert!(loaded.iter().all(|(_, text)| text == "original 한글"));
        assert_eq!(catalog.metadata_with_prefix(PREFIX).unwrap(), changed);
        drop(occupied);
        let restored = restore(&catalog, &loaded).unwrap();
        assert_eq!(restored.len(), 3);
        assert!(restored.get(&removed).is_none());
        assert_eq!(restored[&replaced], "replacement");
        assert_eq!(restored[&added], "new description");
        assert!(std::ptr::eq(
            restored.get(&retained).unwrap(),
            loaded.get(&retained).unwrap()
        ));
        drop((loaded, restored));
        assert_eq!(exact.used(), 0);
    }
}
