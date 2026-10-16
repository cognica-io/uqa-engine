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
use uqa_sql::{catalog::dependencies::ObjectAddress, SQLError};
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
pub type CommentRegistry = BTreeMap<CommentAddress, String>;
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

pub fn restore(catalog: &dyn CatalogFacade) -> StorageBackendResult<CommentRegistry> {
    let mut registry = CommentRegistry::new();
    for (name, description) in catalog.metadata_with_prefix(PREFIX)? {
        let object = address(&name)
            .ok_or_else(|| StorageBackendError::Other("invalid catalog comment address".into()))?;
        if description.is_empty() || description.contains('\0') {
            return Err(StorageBackendError::Other(
                "invalid catalog comment text".into(),
            ));
        }
        registry.insert(object, description);
    }
    Ok(registry)
}

pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &CommentRegistry,
    mut committed: Arc<CommentRegistry>,
) -> StorageBackendResult<Arc<CommentRegistry>> {
    let addresses = current
        .keys()
        .chain(committed.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    for object in addresses {
        if catalog.map_or(Ok(false), |catalog| {
            catalog.metadata_has_private_changes(&key(object))
        })? {
            if let Some(description) = current.get(&object) {
                Arc::make_mut(&mut committed).insert(object, description.clone());
            } else {
                Arc::make_mut(&mut committed).remove(&object);
            }
        }
    }
    Ok(committed)
}

/// The caller retains the addressed object's lock and existing definition transaction.
pub fn set(
    publication: &dyn CommentPublication,
    object: ObjectAddress,
    description: Option<&str>,
) -> Result<(), SQLError> {
    let object = CommentAddress::from(object);
    let description = description.filter(|text| !text.is_empty());
    let mut registry = publication.comments().clone();
    if let Some(catalog) = publication.comment_catalog() {
        match description {
            Some(text) => catalog.set_metadata(&key(object), text),
            None => catalog.delete_metadata(&key(object)),
        }
        .map_err(|error| uqa_sql::catalog::errors::storage_error("catalog comment", &error))?;
    }
    if let Some(text) = description {
        registry.insert(object, text.to_owned());
    } else {
        registry.remove(&object);
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
        .collect::<Vec<_>>();
    drop(comments);
    if addresses.is_empty() {
        return Ok(false);
    }
    let mut registry = publication.comments().clone();
    for object in addresses {
        if let Some(catalog) = publication.comment_catalog() {
            catalog.delete_metadata(&key(object)).map_err(|error| {
                uqa_sql::catalog::errors::storage_error("remove catalog comment", &error)
            })?;
        }
        registry.remove(&object);
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
    use std::cell::RefCell;
    use uqa_storage::{KeyValueCatalog, MemoryKeyValueStore};

    struct Publication {
        catalog: KeyValueCatalog,
        comments: RefCell<CommentRegistry>,
    }
    impl CommentPublication for Publication {
        fn comments(&self) -> Box<dyn Deref<Target = CommentRegistry> + '_> {
            Box::new(self.comments.borrow())
        }
        fn comment_catalog(&self) -> Option<&dyn CatalogFacade> {
            Some(&self.catalog)
        }
        fn publish_comments(&self, registry: CommentRegistry) {
            *self.comments.borrow_mut() = registry;
        }
    }

    #[test]
    fn object_descriptions_restore_replace_and_drop_by_identity() {
        let publication = Publication {
            catalog: KeyValueCatalog::new(Arc::new(MemoryKeyValueStore::new())),
            comments: RefCell::default(),
        };
        let first = ObjectAddress::whole(uqa_sql::catalog::dependencies::TYPE_CLASS, 20_000);
        let second = ObjectAddress::whole(uqa_sql::catalog::dependencies::TYPE_CLASS, 20_001);
        set(&publication, first, Some("original\n설명")).unwrap();
        set(&publication, second, Some("independent")).unwrap();
        assert_eq!(
            restore(&publication.catalog).unwrap(),
            *publication.comments.borrow()
        );
        set(&publication, first, Some("replacement")).unwrap();
        assert_eq!(
            restore(&publication.catalog).unwrap()[&first.into()],
            "replacement"
        );
        remove(
            &publication,
            first.class_id,
            &BTreeSet::from([first.object_id]),
        )
        .unwrap();
        assert_eq!(
            restore(&publication.catalog).unwrap(),
            BTreeMap::from([(second.into(), "independent".into())])
        );
        set(&publication, second, Some("")).unwrap();
        assert!(restore(&publication.catalog).unwrap().is_empty());
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
            assert!(restore(&catalog).is_err(), "{name}");
        }
    }
}
