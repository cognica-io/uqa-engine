//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn persisted_ivf_parameters_reject_invalid_values() {
    let invalid = BTreeMap::from([("lists".to_string(), "not-a-number".to_string())]);
    assert!(IVFIndexParams::from_catalog_map(&invalid).is_err());

    let zero = BTreeMap::from([("probes".to_string(), "0".to_string())]);
    assert!(IVFIndexParams::from_catalog_map(&zero).is_err());
}

#[test]
fn persisted_ivf_aliases_preserve_defaults_and_reject_platform_overflow() {
    assert_eq!(
        IVFIndexParams::from_catalog_map(&BTreeMap::new()).unwrap(),
        IVFIndexParams::default()
    );
    let overflow = (u128::from(u64::MAX) + 1).to_string();
    for name in [
        "lists",
        "nlist",
        "probes",
        "nprobe",
        "train_threshold",
        "train-threshold",
        "min_train",
    ] {
        let parameters = BTreeMap::from([(name.to_uppercase(), "7".into())]);
        let parsed = IVFIndexParams::from_catalog_map(&parameters).unwrap();
        let defaults = IVFIndexParams::default();
        let expected = match name {
            "lists" | "nlist" => IVFIndexParams {
                nlist: 7,
                ..defaults
            },
            "probes" | "nprobe" => IVFIndexParams {
                nprobe: 7,
                ..defaults
            },
            _ => IVFIndexParams {
                train_threshold: 7,
                ..defaults
            },
        };
        assert_eq!(parsed, expected);
        for value in ["-1", overflow.as_str()] {
            let parameters = BTreeMap::from([(name.into(), value.into())]);
            let error = IVFIndexParams::from_catalog_map(&parameters).unwrap_err();
            assert!(error.to_string().contains(&format!(
                "invalid persisted IVF parameter `{name}` value `{value}`"
            )));
        }
    }
}
