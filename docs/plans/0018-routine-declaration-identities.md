# Routine declaration type identities

Issue [#967](https://github.com/cognica-io/uqa-engine/issues/967) restores existing routine behavior while delivering original #638 record 65. It does not complete the remaining range declaration, constructor, lifecycle/operator or provider/language requirements.

## Root cause and ownership

The new shell-type probe accepted a textual type name before SQL resolved a structured `table.column%TYPE` reference. Re-parsing that reference as a standalone type raised `42601` at `%`. Parameter default analysis later repeated the same probe after SQL had replaced an enum/domain name with its catalog identity, raising `42601` at `#`. Main run [38985134412](https://github.com/cognica-io/uqa-engine/actions/runs/38985134412) reproduced twenty failing existing integration cases identically on Linux and macOS.

SQL owns declaration resolution, shell diagnostics and default analysis. Execution's existing catalog probe and Engine's borrowed adapter remain unchanged. Cargo features, dependencies, storage formats and the public SQL contract do not change.

## Correction and preserved behavior

A structured column reference reaches the existing relation-column resolver without a standalone shell lookup. Ordinary names, including a quoted name containing `%TYPE`, still perform the shell lookup. Every parameter still participates in output-count tracking; declarations retain the original type USAGE and argument/result validation order.

The initial shell lookup returns its selected OID to declaration processing, which lends that identity to default analysis. Defined parameters use their already bound catalog type directly. Default expression analysis still precedes the existing shell/default diagnostic, so NULL, typed expression, language, notice and privilege behavior keep their original ordering. No shell type receives an invented value layout.

## Verification and delivery

Two SQL-owner tests cover structured and quoted dotted references in arguments/results, a quoted ordinary name containing `%TYPE`, shell lookup counts, notices, USAGE and SQL-language rejection. All twenty originally failing integration cases and four adjacent shell catalog/provider cases pass locally, including independent PostgreSQL defaults and definition fixtures, exact overload selection, stored dependencies, domain arrays and persistent reopening. Existing fixture expectations are unchanged. Final validation passes all 26 focused tests, strict SQL library/test Clippy, rustfmt and repository ownership, dependency, harness, hygiene and source policies.

Automatic run [38995505788](https://github.com/cognica-io/uqa-engine/actions/runs/38995505788) passes all 903 checks / 1,311 cases and eight independent-process histories on each of Linux and macOS, with zero failures, errors or skips. The reports select source `d9001786f5722b9930706e2e6cb0ed1d52167988` through tested merge `095e5ddcca0da53c2cb5a6a60b12a8b0440cdc84`, with identical tree `87e1dd2a7b9eec62766a6f6bae66ac483364cbdf`; manifest and selected-source hashes match. Current-main integration changes only policy and plan Markdown, preserving every tested product, test, fixture, dependency and workflow input. No timing measurement or performance speedup is claimed.
