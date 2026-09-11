//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_sql::ast::SequenceDataType;

fn target() -> NextvalTarget {
    let mut state = SequenceState::initial(1, 2, SequenceDataType::BigInt);
    state.definition_generation = [2; 16];
    NextvalTarget {
        name: "public.renamed".into(),
        relation: RelationIdentity::new("public", "renamed"),
        object_id: [3; 16],
        state,
        temporary: false,
    }
}
fn cache(target: &NextvalTarget) -> SessionSequenceCache {
    SessionSequenceCache {
        object_id: target.object_id,
        definition_generation: target.state.definition_generation,
        next_value: 11,
        remaining: 3,
        autonomous: true,
    }
}
#[test]
fn renamed_sequence_consumes_its_stable_identity_cache_and_rekeys_remaining_values() {
    let target = target();
    let old = RelationIdentity::new("public", "before_rename");
    let mut caches = BTreeMap::from([(old.clone(), cache(&target))]);
    assert_eq!(
        SequenceValueContext::take_cached_nextval(&target, &mut caches).unwrap(),
        Some((11, true))
    );
    assert!(!caches.contains_key(&old));
    let remaining = caches[&target.relation];
    assert_eq!(remaining.object_id, target.object_id);
    assert_eq!(
        remaining.definition_generation,
        target.state.definition_generation
    );
    assert_eq!(
        (
            remaining.next_value,
            remaining.remaining,
            remaining.autonomous
        ),
        (13, 2, true)
    );
    assert_eq!(
        SequenceValueContext::take_cached_nextval(&target, &mut caches).unwrap(),
        Some((13, true))
    );
    assert_eq!(
        SequenceValueContext::take_cached_nextval(&target, &mut caches).unwrap(),
        Some((15, true))
    );
    assert!(caches.is_empty());
}
#[test]
fn stale_identity_or_definition_discards_only_the_selected_cache_entry() {
    let target = target();
    let unrelated = RelationIdentity::new("public", "unrelated");
    let mut other = cache(&target);
    other.object_id = [9; 16];
    for stale_identity in [true, false] {
        let mut stale = cache(&target);
        if stale_identity {
            stale.object_id = [8; 16];
        } else {
            stale.definition_generation = [8; 16];
        }
        let mut caches =
            BTreeMap::from([(target.relation.clone(), stale), (unrelated.clone(), other)]);
        assert_eq!(
            SequenceValueContext::take_cached_nextval(&target, &mut caches).unwrap(),
            None
        );
        assert_eq!(caches.len(), 1);
        assert!(caches[&unrelated] == other);
    }
}
#[test]
fn corrupt_cached_overflow_is_rejected_without_publishing_a_replacement() {
    let target = target();
    let mut corrupt = cache(&target);
    corrupt.next_value = i64::MAX;
    let mut caches = BTreeMap::from([(target.relation.clone(), corrupt)]);
    let error = SequenceValueContext::take_cached_nextval(&target, &mut caches).unwrap_err();
    assert!(
        matches!(error, SequenceValueError::Internal(message) if message == "cached sequence `public.renamed` value overflow")
    );
    assert!(caches.is_empty());
    for (increment, bound, value) in [(1, "maximum", 20), (-1, "minimum", -20)] {
        let mut state = target.state;
        state.increment = increment;
        state.min_value = -20;
        state.max_value = 20;
        assert!(
            matches!(exhausted("ids", state), SequenceValueError::Exhausted { name, bound: actual, value: actual_value } if name == "ids" && actual == bound && actual_value == value)
        );
    }
}
