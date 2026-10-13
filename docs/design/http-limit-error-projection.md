# HTTP limit error projection

## Domain and observations

Let a request execution produce either a successful SQL result R or a rejected response E = (status, code, request identity, optional diagnostic, optional limit document). The existing client validates response media type, bounded complete bytes and request identity before returning R or E. This change introduces no SQL, algebraic, session or transaction operator. It adds a partial projection V from a limit document and code to either a validated numeric tuple or absence.

The tuple contains a kind from the seven documented constants, integers L and A with A > L, a Boolean lower-bound marker B, and an optional executing limit X. All integer components are at most 2^53 - 1. L is positive except for the parameters kind, which permits zero. The code must match the fixed code for its kind. Body and admission kinds require B = true; other kinds require B = false. Admission kinds require positive X, and other kinds prohibit X. The document contains only the declared fields. Any failed condition maps to absence. Validation never incorporates an arbitrary server message, query, identifier, hint or unknown field into the tuple.

## Preservation argument

For every accepted request, the new success decoder is identical to the predecessor, so its result R and state effects are equal. For every rejected request, the status, code and request identity are unchanged. The projection reads already bounded response bytes and performs no provider request, retry, batch split or state mutation. Thus each request has the same execution trace and atomicity, and composition of successive SQL calls has the same success or failure boundary and database effects. Erasing the optional tuple from the new error recovers the predecessor observation exactly. Missing or invalid metadata maps to absence and preserves the predecessor error text.

For valid metadata, display is a function solely of the closed kind, bounded integers and B. When B is true, display states `at least A`; it never asserts an exact observed count. The extra X is displayed only for an admission kind. Every integer within the chosen range has an exact IEEE 754 binary64 representation, so Rust, Python and JavaScript expose equal numeric observations without rounding. Larger wire integers are rejected by the projection. No new algebraic rewrite is introduced: in particular, replacing one atomic batch with multiple requests is prohibited.

## Implementation correspondence

`uqa-client` owns `HttpLimitKind`, `HttpLimit`, envelope projection and Rust error formatting. Python converts the already validated tuple to its ordinary dictionary shape. The standalone Node.js and browser transports validate the same wire tuple and return frozen JavaScript objects. Public package file inventories include their validators. The owning HTTP integration harnesses test complete real responses, exact batch counts, body lower bounds, admission evidence, request identity mismatch, malformed metadata, numeric bounds, privacy and absence of automatic replay. Existing SQL, batch and stream tests continue to cover the unchanged execution paths.
