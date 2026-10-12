# AirPrinter

[View the HTML project documentation](doc/index.html)

The current stable release is **2.0.0.stable**. The installer uses the SemVer-compatible version `2.0.0`. The `main` branch uses the direct-broadcast design, which has been tested with a physical printer: each installed Windows printer queue (USB, wired network, or paired Windows queue) is advertised to the local network. Each shared queue has its own discovery identity and IPP resource path, and received jobs are sent directly to the matching Windows queue. The first successful physical-print snapshot is preserved as `v1.0.0`; the fixed virtual-entry design is retained on the `virtual-gateway-printer` branch.

## Use AirPrinter

1. Install the printer driver in Windows and confirm that the printer works locally.
2. Start AirPrinter and enable sharing for each Windows printer queue you want to share.
3. Connect the iPhone, iPad, or Mac and the PC to a local network that allows them to communicate. Select the queue from the device's system print dialog.
4. You can stop sharing one queue independently. Other queues continue running, and jobs already received remain bound to their original queue.

By default, closing the window sends it to the system tray and releases the WebView while the printing service keeps running. Click the tray icon to recreate the window. When **Start at login** is enabled, AirPrinter starts with `--background` and does not load the frontend window. With the window open, Windows queues refresh every 30 seconds; in background-only mode, they refresh every five minutes to reduce PowerShell queries and frontend memory use.

For normal printing, choose a shared queue in the device's system print dialog, set options, and print. The PC receives the job, processes it, and sends it to that Windows queue automatically. You do not need to download the document, open a viewer, or confirm printing again. A browser test page is only for connection diagnostics; it is not part of normal printing.

Virtual queues that require local interaction, such as PDF, XPS, OneNote, and Fax queues, are excluded from the normal sharing list. Simulated tests explicitly use Microsoft Print to PDF and specify an output file, so no save dialog is needed.

## Printing flow

```text
Windows queues and driver capabilities
  → Per-queue DNS-SD services and /ipp/print/<id> endpoint
  → IPP Print-Job or Create-Job + Send-Document
  → Route to the Windows queue bound to that resource path
  → Render PDF / JPEG / URF / PWG Raster pages
  → GDI / Spooler for the selected Windows queue
  → IPP queries, cancellation, and job status
```

- Rust `mdns-sd` advertises `_ipp._tcp`, `_universal._sub._ipp._tcp`, and a port-0 `_printer._tcp` identification service for each shared queue. The current standards subtype and LOC deviations are tracked in the [conformance plan](doc/conformance-plan.md).
- Each queue has a unique `/ipp/print/<id>` URI. The compatibility alias `/ipp/print` is accepted only when exactly one queue is shared.
- IPP default-port semantics are followed: an `ipp://` URI may omit port 631, while the HTTP `Host` header is still checked against the actual listening port. Bonjour `.local` names and local IP addresses are both valid destinations.
- While the window is open, Windows queue state and driver capabilities synchronize every 30 seconds. In background-only mode, synchronization runs every five minutes. Sharing selections are stored in the user's configuration directory; pending documents are not copied there.
- PDF is handled by the Windows `Windows.Data.Pdf` API. JPEG uses Windows image decoding. URF/PWG Raster supports the advertised sGray8 and sRGB24 formats and is submitted silently through GDI.
- Supported operations: Print-Job, Validate-Job, Create-Job, Send-Document (single document), Get-Job-Attributes, Get-Jobs, and Cancel-Job.
- AirPrinter advertises the color, duplex, copy-count, and media-size capabilities reported by the driver. A4, A5, Letter, and Legal use standard names; other driver paper sizes receive custom media names based on their dimensions.
- Color printers default to color output according to Windows driver capabilities. The IPP attributes `which-jobs-supported` and `multiple-operation-timeout` use their standard names. DNS-SD TXT records stay within the recommended 400-byte limit.
- Supported options include portrait or landscape orientation, contiguous or non-contiguous page ranges, and fit-to-page, fill, original-size, and automatic scaling. Pages are numbered from 1 in the received document. PDF renders only selected pages; JPEG, URF, and PWG Raster pages can also be selected. Borderless printing, binding, and multi-document jobs are not claimed.
- The job name and copies, duplex, color, media, quality, orientation, page-range, and scaling options submitted by a phone use the same background print flow. Options from Create-Job are retained when Send-Document arrives. Original-size or fill modes may crop content outside the printable area. Invalid page ranges and options unsupported by the driver return errors.
- AirPrinter initially reports a received job as pending. Driver failures are reported as aborted; after submission to the Windows queue, its status is tracked. A job leaving the Windows queue means queue processing has ended; it does not independently prove that paper came out.
- Documents are staged in a randomly named temporary directory and cleaned up after submission succeeds or fails. AirPrinter does not open a browser, default viewer, or print dialog, and does not change the Windows default printer.

## IPP and AirPrint compatibility

Discovery uses DNS-SD/mDNS, and job transfer uses IPP over HTTP. URF is implemented as a document format for Apple-client interoperability; it does not replace IPP with a private transport protocol. The project also implements the PWG Raster path described in `doc/cs-ippeve11-20200515-5100.14.pdf`.

The project does **not** claim full IPP Everywhere 1.1 certification or guarantee compatibility with every Apple OS version. To preserve the observed Apple discovery path, it currently advertises `ipp-features-supported=ipp-everywhere`; however, the MUST requirements for `_print._sub._ipp._tcp`, LOC, and PDF overrides are not yet complete. This attribute is not proof of full conformance. The user has confirmed one successful print from an Apple system to a physical printer using `direct-printer-broadcast`; more OS versions, formats, and print options still need acceptance testing.

- [IPP Everywhere / PWG](https://www.pwg.org/ipp/everywhere.html)
- [Apple documentation on AirPrint DNS-SD subtypes and URF](https://support.apple.com/guide/deployment/dep3b4cf515/web)
- [RFC 6763: DNS-Based Service Discovery](https://www.rfc-editor.org/rfc/rfc6763)

## Network and firewall

The HTTP server currently prefers to listen on IPv4 `0.0.0.0:631`. If Windows has already reserved that port, AirPrinter selects an available port from 8631–8699 and advertises the actual port in the DNS-SD SRV record. The firewall script covers both port ranges. AirPrinter advertises addresses on usable IPv4 interfaces and does not publish unreachable IPv6 services. The PC can connect to the router over Ethernet while Apple devices use Wi-Fi, provided the local network allows communication and UDP 5353 multicast. Guest networks, AP isolation, and VLAN isolation may require separate configuration.

Listening on TCP 631 does not itself require Windows administrator rights; changing firewall rules does. The app's **Allow LAN access** button requests administrator authorization and creates inbound TCP/UDP rules for this program limited to the local subnet, covering both Private and Public network profiles. Alternatively, run this command from an Administrator PowerShell prompt:

```powershell
.\scripts\enable_firewall.ps1 -Program (Resolve-Path .\src-tauri\target\debug\airprinter.exe)
```

For an installed release, replace the path with the installed executable's path. AirPrinter changes the firewall only when the user clicks the button or runs the script manually. IPP has no authentication. When enabled on a Public network, other devices on the same subnet can submit print jobs; use it only on a trusted local network.

## Build

```powershell
pnpm install
pnpm tauri dev
pnpm tauri build
```

To build only the frontend, run `pnpm build`. If a newer pnpm version tries to reinstall an existing dependency directory during verification, you can check the existing dependencies with `pnpm --config.verify-deps-before-run=false run build`.

The backend uses Windows PowerShell, .NET Framework's System.Drawing, the Windows PDF API, and installed printer drivers. No additional PDF reader is required.

## Windows releases

Pushing a tag such as `v2.0.0.stable` runs the GitHub Actions Windows release workflow. It verifies that the tag matches the application version, builds with the x64 MSVC target, creates both MSI and NSIS installers, and uploads them to the corresponding GitHub Release.

The current installers are not code-signed. Windows 11 may therefore show an unknown-publisher or SmartScreen warning. Code signing can be added later by configuring a trusted certificate in repository secrets.

## Repeatable diagnostics and checks

To diagnose phone-to-PC document transfer, start receive-only diagnostic mode:

```powershell
src-tauri\target\debug\airprinter.exe --receive-only "E:\Tauri-Rust-Printer\src-tauri\target\received-documents"
```

The directory must be an absolute path. Received documents are saved there and jobs remain pending; the printer driver is not called. Exit and start AirPrinter normally to resume automatic printing. This mode is for validating document reception, not normal use.

In receive-only mode, a phone can upload a test document to `http://<PC-LAN-address>:631/receive` (or HTTPS on the same port). The job is saved as pending without calling the printer driver. This endpoint diagnoses the receive path and is not part of normal printing. In normal mode, AirPrinter processes and submits the job automatically; users do not need to download it, open a viewer, or print it manually.

From an Administrator PowerShell prompt, run `scripts/capture_print_chain.ps1 -OutputDirectory "E:\Tauri-Rust-Printer"` to capture traffic for a limited 90-second period. It records phone traffic on ports 631/8631, mDNS for the local subnet, print/USBMon events, and queue status. Output is written to `target/print-capture/`. Analyze a capture with `python -X utf8 scripts/analyze_print_capture.py <network.pcapng>`. USBMon output contains driver events, not raw USB packets. Captures may contain document data; keep them local. They are ignored by Git.

```powershell
# Protocol, parsing, and routing checks
cargo test --manifest-path src-tauri/Cargo.toml --locked
python scripts/diagnose_mdns.py --self-test
powershell.exe -NoProfile -File scripts/check_print_options.ps1
# Windows page-selection integration test; outputs only to Microsoft Print to PDF without a save dialog
powershell.exe -NoProfile -File scripts/check_print_options.ps1 -Simulate

# Local simulation using Microsoft Print to PDF
cargo test --manifest-path src-tauri/Cargo.toml --locked -- --include-ignored --nocapture

# JavaScript virtual client and host-only virtual network adapter; does not submit a Print-Job
cargo build --manifest-path src-tauri/Cargo.toml --example simulate_lan --locked
node scripts/simulate_airprint.mjs 172.22.48.1

# Check real broadcasts from another computer on the LAN; does not submit a print job
python scripts/diagnose_mdns.py --seconds 8
# Specify the local LAN interface address when the PC has multiple adapters
python scripts/diagnose_mdns.py --interface 192.168.1.10 --seconds 8
```

Existing local tests cover independent queue routing, IPP attributes, and virtual print output. Results for the current changes are recorded in the audit for that run. These checks do not demonstrate a successful native print submission from a real phone; real Apple-device submission and physical output have not been fully verified.

`http_to_windows_pdf_end_to_end` submits URF through the HTTP IPP test entry point, queries the job until Windows queue processing ends, and saves a PDF for inspection under `src-tauri/target/airprint-simulation/`. `windows_raster_spool_smoke` checks all four document formats, including rendering a PDF again for printing. The mDNS test checks service resolution and deregistration.

The JavaScript script requires a host-only virtual network adapter; replace its argument with that adapter's IPv4 address. The Rust simulated printer limits its IPP listener and mDNS to that interface. A separate Node.js process simulates the DNS-SD and IPP client. This PC's Hyper-V default switch does not loop mDNS multicast back to the host, so the script explicitly marks its DNS-SD portion as an isolated UDP loopback simulation. IPP attribute queries, parameter validation, and Host rejection still use the real Rust backend. It does not submit a print job to a fictitious queue.

These tests show that the local implementation can run; they do not replace acceptance testing across a Wi-Fi router, firewall, Apple system print UI, and physical printer. Resource limits are 64 MiB per upload, 512 MiB cumulative raster decoding, 500 pages maximum, 120 seconds per Windows render/submission, four jobs processing at once, and 32 unfinished jobs. Requests over a limit return an error instead of allowing unbounded growth.

## Support and donations

For support, please open an issue in this repository. To donate via PayPal, send a payment to **licher255@outlook.com**.

## License

AirPrinter is licensed under the [MIT License](LICENSE).
