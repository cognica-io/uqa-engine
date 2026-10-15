#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Select only the features required by a benchmark in its own source revision."""


def required_features(metadata, package, benchmark):
    owner, = (item for item in metadata["packages"] if item["name"] == package)
    target, = (item for item in owner["targets"]
               if item["name"] == benchmark and "bench" in item["kind"])
    return [f"{package}/{feature}" for feature in target.get("required-features", [])]


def feature_arguments(metadata, package, benchmark):
    features = required_features(metadata, package, benchmark)
    return ["--features", ",".join(features)] if features else []
