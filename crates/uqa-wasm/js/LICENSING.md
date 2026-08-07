# UQA-RS Licensing Policy

Policy version 1.0, effective 2026-08-03.

UQA-RS is open-source software licensed under the GNU Affero General Public License version 3 only (`AGPL-3.0-only`). Cognica, Inc. also provides two optional additional permissions and offers separate commercial terms. These alternatives do not revoke or reduce any right granted by the AGPL.

The legal terms are the [AGPL license](LICENSE) and the applicable exception text. This document explains how the available paths are intended to work; it does not replace those terms.

## Public-core commitment

Code accepted into the public UQA-RS core will remain available under at least one license approved by the Open Source Initiative. A commercial license for a release does not withdraw, terminate, or narrow rights already granted for that release under an open-source license.

## Available paths

| Path | Who may use it | License of an independent application | License of UQA-RS and modifications to UQA-RS |
| --- | --- | --- | --- |
| AGPL | Anyone, including commercial users | Governed by the AGPL to the extent the AGPL applies | AGPL-3.0-only |
| FOSS exception | A complete qualifying open-source application | May remain under its OSI-approved license | AGPL-3.0-only |
| Noncommercial exception | A qualifying personal, educational, academic, or charitable application | May remain under terms chosen by its author | AGPL-3.0-only |
| Commercial license | A customer with a signed agreement from Cognica | May remain proprietary as agreed | May remain proprietary as agreed |

Commercial activity is not prohibited by the AGPL. A business may use UQA-RS without paying a fee if it complies with the AGPL or qualifies for the FOSS exception. A commercial license is an alternative for a customer that needs rights beyond those public terms.

## 1. AGPL-3.0-only

The AGPL is the default license for the complete repository and all published packages unless a file or package states otherwise. It permits use for any purpose, including commercial use, subject to its conditions.

Use this path when the complete deployment and distribution can comply with the AGPL. The two public exceptions are optional; no one is required to use them.

## 2. FOSS exception

The [UQA-RS FOSS Exception](LICENSES/UQA-FOSS-EXCEPTION-1.0.txt) permits a qualifying application distributed with complete source under an OSI-approved license to combine with UQA-RS without changing the license of the application's independent code solely because of that combination.

The exception covers ordinary integration mechanisms, including static or dynamic linking, Rust dependencies, foreign-function interfaces, language bindings, imports, and WebAssembly bundling. It does not relicense UQA-RS as Apache-2.0, MIT, MPL-2.0, or the application's license. UQA-RS, copied UQA-RS implementation code, and modifications to UQA-RS remain under the AGPL. The application's own license must also permit the proposed combination; OSI approval alone does not guarantee license compatibility in every direction.

A commercial organization may use this exception when the complete application genuinely qualifies as open-source software. Publishing only a thin wrapper while keeping the effective application proprietary does not qualify.

## 3. Noncommercial exception

The [UQA-RS Noncommercial Application Exception](LICENSES/UQA-NONCOMMERCIAL-EXCEPTION-1.0.txt) permits a qualifying personal, educational, academic-research, or charitable application to keep its independent application code under terms chosen by its author.

The exception does not permit UQA-RS itself or modifications to UQA-RS to become proprietary. It does not cover paid services, advertising or subscription-supported services, customer workloads, commercial product development, consulting, or research performed for a for-profit sponsor that receives proprietary or exclusive results.

For-profit evaluation and proof-of-concept use are not automatically covered by this exception. Contact Cognica for written evaluation or commercial terms when the ordinary AGPL path is unsuitable.

## 4. Commercial license

Cognica offers separate commercial terms for proprietary applications, closed modifications, software-as-a-service deployments, OEM distribution, devices, appliances, and customers that require contractual support, warranties, or other assurances. See [COMMERCIAL.md](https://github.com/cognica-io/uqa-rs/blob/main/COMMERCIAL.md).

A commercial license is granted only through a separate signed agreement. The public repository does not itself grant commercial-license rights.

## Common cases

| Scenario | Expected path |
| --- | --- |
| An AGPL service publishes its complete corresponding source | AGPL |
| A company distributes a complete Apache-2.0 application that embeds UQA-RS | FOSS exception |
| An individual builds a private, unpaid learning project | Noncommercial exception |
| A university conducts independent academic research without proprietary sponsorship | Noncommercial exception |
| A company evaluates UQA-RS internally under the AGPL | AGPL |
| A proprietary SaaS product keeps its application and UQA-RS changes closed | Commercial license |
| An OEM embeds UQA-RS in a closed product or device | Commercial license |
| A proprietary product publishes only a thin open-source wrapper around UQA-RS | AGPL or commercial license; the FOSS exception does not apply |

## Third-party software

These terms apply only to material for which Cognica has authority to grant the stated rights. Dependencies, bundled libraries, data sets, models, and other third-party material remain governed by their own licenses and notices. A commercial agreement from Cognica does not replace third-party obligations.

## Package metadata and distributions

Cargo, Python, Node.js, and WebAssembly package metadata reports `AGPL-3.0-only` because the AGPL is the base open-source license. The two exceptions are additional permissions, not a choice to relicense UQA-RS under the application's license.

Official source and registry distributions must carry a licensing notice that identifies this policy and the applicable exception texts. A copy that does not carry an exception notice remains available under the AGPL but does not grant that optional exception. Redistributors relying on an exception must include the applicable exception text as required by that exception.

## Contributions

Alternative licensing requires Cognica to retain sufficient rights in every accepted contribution. External code contributions are governed by [CONTRIBUTOR_POLICY.md](https://github.com/cognica-io/uqa-rs/blob/main/CONTRIBUTOR_POLICY.md). Issues, design discussion, bug reports, and other feedback that do not contribute copyrightable code do not require a contributor agreement.

## Questions

Licensing questions and requests for evaluation or commercial terms may be sent to `jaepil@cognica.io`. Obtain independent legal advice for a definitive interpretation of how a license applies to a particular product or deployment.
