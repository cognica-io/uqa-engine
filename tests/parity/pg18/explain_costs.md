# EXPLAIN cost output reference

The forty cases independently record COSTS defaults, explicit and duplicate switches, all four output formats, row estimates for fixed VALUES/constant inputs, and analyzed SQL fixed-width output. They do not require native physical costs or plan node names to equal PostgreSQL’s cost model. Costs remain native estimates; their nonnegative finite values, blocking startup and LIMIT behavior are checked in Planner and Engine tests.

Capture used PostgreSQL 18.4 (Debian 18.4-1.pgdg13+1), aarch64, image `sha256:bc8a94e4a938576533c0f0e8ad6f2e8c67d5c5facef05005a5e5cf9adce2fe25`. Set `PG_EXPLAIN_COMMAND` to a JSON array containing the desired psql command and connection arguments, then run `python3 tests/parity/pg18/capture_explain_costs.py`. The capture only runs read-only constant explanations.

Capture script SHA-256 is `22d33ad8a0be264f6a2d130ef9d0c5c9ebdcad123d6d8b3b7f8374ef816e7257`; fixture SHA-256 is `a51c4d5cef0933e765dd738aab3cc07b468ef06d76d5f6be2f22fbb64acdbb92`.
