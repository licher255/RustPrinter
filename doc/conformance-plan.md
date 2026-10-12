# Conformance Audit and Acceptance Plan for the Two Publishing Models

Baseline: `cs-ippeve11-20200515-5100.14.md` (PWG 5100.14-2020). This document records implementation evidence and outstanding work. The presence of an attribute or a passing local test does not constitute IPP Everywhere certification or successful printing from a real Apple device.

## Boundaries of the Two Branches

| Branch | Published identity | Job routing | Must be verified separately |
| --- | --- | --- | --- |
| `direct-printer-broadcast` | Each Windows queue has its own DNS-SD instance, UUID, and `/ipp/print/<id>` | Submit to the physical queue associated with the URI; multiple queues coexist | Both queues broadcast simultaneously; concurrent submissions; the remaining queue stays available after one is withdrawn; the default `/ipp/print` alias works only when there is one queue |
| `virtual-gateway-printer` | Fixed AirPrinter identity, UUID, and `/ipp/print` | Lock the current physical target at Create-Job / Print-Job time; existing jobs continue to use their original target after a switch | Identity remains unchanged during switching; jobs do not cross queues; withdrawal while offline; old physical queue URIs match only the current target |

The existing commits for `ipp/server.rs` and `mdns_broadcaster.rs` are identical on both branches. The shared conformance fixes were applied to both branches separately and regression-tested on each; behavior must not be assumed identical solely because the source is shared.

## MUST / SHOULD Audit Items

| Specification clause | Current evidence and issue | Acceptance criteria |
| --- | --- | --- |
| 4.2.1–4.2.2 DNS-SD | The default instance-name hash has been removed, and this process rejects name collisions; `_print._sub._ipp._tcp` and the corresponding IPPS subtype are still missing, and automatic renaming after a name collision is not implemented | The same base instance is discoverable through both Apple and specification subtypes; PTR records point to the same SRV/TXT; both subtypes are withdrawn when sharing stops; append a distinguishing suffix only when a collision occurs |
| 4.2.3 LOC | The current `mdns-sd` interface does not publish LOC, and no user-configurable location is available | Provide location configuration and publish a parseable LOC for each relevant instance; encode unknown precision as specified; retain this as an explicit deviation until implementation is possible |
| 4.2.4 TXT | `rp`, `pdl`, `TLS`, and TXT length have tests | Capture actual mDNS packets and confirm that `rp` appears within the first 400 bytes, total length limits are met, and IPP/IPPS ports agree with `printer-uri-supported` |
| 5.1.1–5.1.3 HTTP | A unique Host is validated consistently; local regressions for conditional GET and 304 on icons, and `no-cache` on IPP POST, pass | Review Host, Last-Modified, and conditional responses in real-device traffic; route any newly exposed resources through the same validation |
| 5.2, Table 4 operations | Eleven operation codes are declared and some have protocol tests; `Identify-Printer` only logs the request, so its response semantics need verification | Each declared operation has positive and negative cases and replay tests using actual IPP serialization, consistent with job state and Windows cancellation behavior |
| 5.3–5.8, Tables 5/7/8/10/11/12 | Existing tests check that attributes are present, but do not verify each value, its applicability conditions, or actual execution | Create a table-driven test for each item: value type, units, default, supported range, conditions, and errors for unsupported options; declarations must match driver capabilities and actual backend behavior |
| 9.3 PDF conditional requirements | Non-empty `document-password` and any `overrides` are now explicitly rejected; the false `overrides-supported` claim has been withdrawn. PDF conditional `overrides` functionality is still missing | Implement actual Windows output with page-level overrides and positive/negative protocol tests; restore capability declarations that match the implementation |
| 5.3/5.4 device facts | Speed and margins that have not been measured use `unknown` (0x12); the IPP model and DNS-SD `ty`/`product` both identify the actual software bridge | Refine values when they can be obtained from the actual driver; values and units must match print execution |
| 7.1 logical-device flags | Both models report `ipp-everywhere` and `ipp-everywhere-server` | Keep logical-device flags consistent; do not claim full conformance until all other mandatory items are satisfied |
| 8, 10, 11 strings/URIs/security | Coverage is not yet established for Unicode control-character filtering, URI sources, and all HTTP Host paths | Replay tests with control characters and multilingual queue names, malicious Host values, IPv4/Bonjour/alternate ports; do not produce invalid or unreachable URIs |

## Implementation Order and Release Gates

1. **Shared protocol layer**: Fix Host and conditional GET handling, review the real semantics of operational attributes, then address DNS-SD dual subtypes and location records. Each change must include its clause, code location, and tests before and after the fix.
2. **Accept each routing model separately**: First test two queues with direct broadcasting, then target switching through the fixed virtual entry point. Regression coverage should include Print-Job, Validate-Job, Create/Send/Close, queries, cancellation, stopping sharing, and recovery.
3. **Local integration**: Run `cargo test --locked`, `cargo check --all-targets --locked`, and `pnpm build`; inspect packet captures for PTR/SRV/TXT/LOC, HTTP headers, and IPP encoding; use only a test virtual queue for automatic output.
4. **Real-device acceptance**: Discover and submit PDF/JPEG, multipage documents, and print options from another Apple device; observe file receipt, the Windows spooler, and physical output as three separate stages. Until this evidence is available, publish only development builds and verified scope; do not label the result as full AirPrint/IPP Everywhere conformance.

The README, project documentation, and test records have been updated separately on both branches; the direct-broadcast branch describes per-queue publishing, and the virtual-entry branch describes a fixed entry point. Final acceptance with a real Apple device and physical printer is still required.
