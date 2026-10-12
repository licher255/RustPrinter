# The Simple Printer Feature That Took Nearly 800 Million Tokens

## A specification-first case study in AI-assisted engineering

AirPrinter began with an ordinary household problem: an older USB printer still worked, but it had no useful network-sharing feature for iPhone, iPad, or Mac. The printer was already connected to a Windows PC, and the home router could carry the network traffic. In principle, the missing piece was small: advertise the Windows queue on the local network, receive an AirPrint-compatible IPP job, and send it to the installed Windows driver.

The final product does exactly that. Each selected Windows printer queue is advertised through DNS-SD/mDNS, receives IPP jobs, renders the supported document formats, and submits them to the matching Windows spooler queue.

Getting there was unexpectedly difficult.

## What happened

From October 2025 through October 2026, I repeatedly used AI coding agents to build and debug the feature. My own estimate is that the effort consumed close to 800 million model tokens across many sessions. That number is an estimate from the project history, not a controlled benchmark.

The most capable Codex runs, including GPT-6 Astra, made substantial progress. They built working components and investigated the system carefully:

- Windows printer discovery and capability inspection
- IPP request parsing and job state tracking
- PDF, JPEG, URF, and PWG Raster processing
- DNS-SD and mDNS advertisements
- HTTP and TLS listeners, including mixed HTTP/TLS routing
- diagnostic web pages and receive-only upload paths
- packet captures, ETW traces, queue sampling, and metadata logs
- repeated tests with my iPhone and a physical printer

The agent often asked me to perform another iPhone action, collected another trace, and closed another part of the loop. We proved that the phone could discover a service. We proved that the server could receive a file through a diagnostic page. We proved that Windows could render and spool the formats. We proved that TLS handshakes and HTTP requests could reach the PC.

But native system printing still stopped before submitting the document.

## Why the closed-loop approach stalled

The debugging loop was locally rational:

1. Observe a failure.
2. Add logging or a diagnostic endpoint.
3. Reproduce the failure on the iPhone.
4. Patch the next visible incompatibility.
5. Repeat.

This approach is excellent when the implementation is mostly correct and one defect blocks the path. It performs poorly when the server's advertised contract is incomplete.

An IPP client does not need to upload a document just because the TCP, TLS, HTTP, and basic IPP layers work. It first evaluates the printer's discovery records, operations, required attributes, value types, supported formats, job template capabilities, and URI behavior. A response can be syntactically valid and still be semantically insufficient. If that contract is incomplete or inconsistent, the client can stop at capability negotiation without producing a convenient error.

The project spent a long time proving individual links in the pipeline while the real gating condition was distributed across a standards document.

## The breakthrough: make the specification readable and executable

The turning point came when I converted the IPP Everywhere 1.1 specification from PDF into Markdown and explicitly instructed the implementation agent to treat every normative **MUST**, **MUST NOT**, **SHOULD**, and conditional requirement as an audit item.

That changed the task from:

> Find the next reason the iPhone might not print.

to:

> Compare the implementation with the complete protocol contract, identify every applicable normative requirement, and prove that each advertised capability exists in the real execution path.

Using Kimi K3 together with DSH, this specification-first pass completed the feature that the earlier iterative debugging had not completed. The important change was not simply using a different model. It was giving the model a representation of the authoritative standard that it could search, quote, cross-reference, and turn into a checklist.

The audit found missing operations and attributes, inconsistent discovery and IPP identity data, hard-coded capabilities, and claims that were not backed by execution. It also forced the project to distinguish between:

- a required attribute being present;
- the attribute having the correct IPP value type;
- the advertised value matching DNS-SD records;
- the backend actually honoring the corresponding print option; and
- a local test passing versus a real Apple client and physical printer succeeding.

After the contract was completed and false capability claims were removed, the direct-broadcast implementation successfully printed from Apple's system print flow to the physical printer.

## PDF remains a difficult source format for coding agents

This project also exposed a practical limitation in AI-assisted standards work: PDF is often a poor machine-working format.

A human can visually navigate a PDF table, footnote, continuation page, or conditional note. An agent may receive extracted text with broken headings, merged columns, missing reading order, detached notes, or tables that no longer preserve the relationship between a requirement and its condition. The information is technically present, but it is expensive to retrieve reliably.

Converting the standard to Markdown made several operations much easier:

- search all normative keywords;
- preserve headings and section boundaries;
- compare required-operation and required-attribute tables with source code;
- link implementation decisions to exact clauses;
- review omissions without repeatedly reopening page images; and
- keep the specification inside the repository beside the implementation and tests.

This does not mean Markdown is always superior to PDF. Figures, typography, signatures, and final publication layout still benefit from PDF. It means standards intended for implementation should also be published in semantic formats such as HTML, Markdown, structured XML, or machine-readable registries.

## The reasoning lesson

People sometimes describe failures like this as a need for “more chain of thought.” I think the more useful lesson is broader: the agent must know what evidence to seek before it starts reasoning deeply.

More reasoning over incomplete evidence can produce a longer debugging loop. A better engineering process is:

1. Identify the authoritative standard or source of truth.
2. Convert it into a representation the agent can navigate reliably.
3. Extract normative requirements into a traceable checklist.
4. Map every claim to implementation code and an executable test.
5. Separate protocol conformance, interoperability, transport, backend execution, and physical output into distinct acceptance gates.
6. Record unresolved deviations instead of inventing plausible capabilities.

The best result came from combining human judgment about which document mattered, document transformation that preserved its structure, and an agent capable of systematically implementing the resulting checklist.

## What this suggests for industry standards

Standards organizations can make AI-assisted implementation more reliable by publishing multiple synchronized representations:

- an authoritative PDF for archival publication;
- accessible HTML with stable anchors;
- Markdown or structured text for repository use;
- machine-readable operation, attribute, enum, and requirement tables;
- explicit identifiers for normative requirements; and
- examples and conformance tests linked directly to those identifiers.

This would help both humans and tools. It would reduce ambiguity, improve accessibility, make diffs reviewable, and let implementers turn standards into tests without reconstructing the document's structure first.

## Result and limits

AirPrinter 2.0.0.stable now provides the household feature that motivated the project: a Windows PC can publish an older printer on the local network so Apple devices can discover it and print through the existing Windows driver.

One successful real-device path is not universal certification. The project still records known IPP Everywhere deviations and keeps protocol conformance separate from interoperability evidence. That distinction is another outcome of the specification-first approach: success is documented precisely, and remaining gaps stay visible.

---

If this project saved an old printer from replacement, you can [buy me a coffee via PayPal](mailto:licher255@outlook.com).

![Buy me a coffee via PayPal](../assets/buy-me-a-coffee.png)
