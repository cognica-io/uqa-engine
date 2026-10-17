# Retained DATE output

Issue [#971](https://github.com/cognica-io/uqa-engine/issues/971) was reproduced while preparing native range routine aliases for original #638 record 65. A prepared composite constant containing integer `-2147483647`, followed by `ALTER TYPE ... ALTER ATTRIBUTE ... TYPE date`, reads the same four owned bytes through DATE output. PostgreSQL 18.4 prints `5881610-07-13`; UQA printed `5877612-06-23 BC`. The independent reference also exposes the same output through a custom date range passed to the int4 canonical implementation. No read crosses the owned tuple boundary.

## Ownership and correction

Core owns the shared integer-day carrier and date output. SQL retains physical interpretation and the PostgreSQL epoch conversion, and Engine tests exercise the public retained-composite adapter. Input validity, infinity sentinels, timestamp behavior, dependencies, feature selections and persisted formats are unchanged. DATE output uses PostgreSQL's unsigned Julian cycle decomposition, including the wrapping arithmetic of its physical date observation. The mathematical signed Gregorian inverse remains an independent test reference for the admitted date interval.

## Preservation argument

For every admitted finite DATE day $d$, $0\le d+2440588<2147483494$. Adding the Julian calendar shift and decomposing into 400-year, four-year and month intervals therefore yields the same calendar date as the signed Gregorian inverse; the property test compares these independent decompositions throughout the admitted interval. Date input still validates that interval, so formatting and reparsing an admitted finite DATE preserve its existing integer carrier. Infinity retains its separate output path.

An in-bounds retained Datum can contain a physical word outside the date input interval after a descriptor change. Its output is the unsigned Julian observation defined by PostgreSQL `date_out` and `j2date`, not an additional accepted SQL date input. Wrapping arithmetic is confined to this physical observation. Comparison keys, equality, storage bytes, row support, multiplicity and ordering are unchanged. Output continues to use the existing admitted string builder under the caller's memory allowance and both cancellation scopes.

The independent Docker PostgreSQL 18.4 reference contains the five public statements from type creation through prepared execution after ALTER TYPE. All five focused Core calendar cases pass, including the captured physical output, output reservation failure/release and agreement with the admitted Gregorian calendar. The public sequence passes on memory, native SQLite, SQLite Key/Value and redb. Core, SQL and Engine library/test Clippy pass with warnings denied; the ownership and dependency checks pass. The performance inventory validates 919 checks and 1,333 required cases, and its thirty verifier tests pass. Automatic delivery is complete as recorded below. Original records 34 and 65 retain their full remaining scope.


## Delivery

[PR #974](https://github.com/cognica-io/uqa-engine/pull/974) is merged as `03e99ac979ee1fb35b6f88792067c872d90a2f13`, closing #971. Automatic performance run [39008847839](https://github.com/cognica-io/uqa-engine/actions/runs/39008847839) passes all 919 checks / 1,333 cases and eight independent-process histories on each of Linux and macOS, with zero failures, errors or skips. Each report's manifest and source hashes match final HEAD `ecfd46144`; tested merge `6b3126765` and the delivered merge have identical tree `8b7fb3376f73c4459b0d89fb934bcb4880e60765`. No timing acceptance is claimed. The single final-ready FDW run [39008852214](https://github.com/cognica-io/uqa-engine/actions/runs/39008852214) passes all six actual platform/feature configurations, with the final input fingerprint verified. The completed worktree and local/remote branch are removed. Original records 34 and 65 remain open for their separate requirements.
