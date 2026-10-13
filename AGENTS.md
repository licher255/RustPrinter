# AirPrinter - Agent Documentation

## Project Overview

AirPrinter is a Tauri-based desktop application that shares Windows USB printers as AirPrint-compatible printers, allowing iPhone/iPad/macOS devices to discover and print directly.

**Key Features:**
- Automatic detection of system printers
- mDNS/Bonjour service broadcasting (AirPrint compatible)
- Support for PDF, JPEG, URF formats
- Duplex printing and multiple copies support
- IPP Everywhere™ protocol implementation

**System Requirements:**
- Windows 10/11 (primary target platform)
- Same local network as iOS/macOS devices
- USB printers with Windows drivers installed

## Technology Stack

### Frontend (TypeScript)
- **Build Tool**: Vite v6.x (port 1420)
- **UI Framework**: Vanilla TypeScript with Tailwind CSS v3 + DaisyUI v5
- **Internationalization**: i18next v25 with browser language detection
- **Tauri API**: `@tauri-apps/api` v2.10+ for Rust backend communication

### Backend (Rust)
- **Framework**: Tauri v2.0
- **i18n**: `rust-i18n` v3 (YAML-based)
- **mDNS**: `mdns-sd` v0.11 for service discovery
- **HTTP Server**: `tiny_http` v0.12 for IPP protocol
- **IPP Protocol**: `ipp` crate v5.4
- **Serialization**: `serde` + `serde_json`

### Network Protocols
- **mDNS**: UDP port 5353 (service discovery)
- **IPP**: TCP port 631 (Internet Printing Protocol)
- **Service Types**:
  - `_ipp._tcp` - Base IPP service
  - `_printer._tcp` - RFC 6763 Flagship Naming (port 0)
  - `_print._sub._ipp._tcp` - IPP Everywhere™ subtype

## Project Structure

```
airprinter/
├── src/                          # Frontend (TypeScript)
│   ├── main.ts                   # Entry point, i18n initialization
│   ├── styles.css                # Tailwind imports + global zoom (0.8)
│   ├── components/
│   │   ├── Header.ts             # (Not actively used - inline in HTML)
│   │   ├── LogPanel.ts           # Log display component
│   │   ├── PrinterList.ts        # Printer list with share/unshare buttons
│   │   └── Settings.ts           # Settings panel (language, close-to-tray, autostart)
│   ├── services/
│   │   ├── printerService.ts     # Tauri invoke wrappers for printer commands
│   │   ├── settingsService.ts    # Tauri invoke wrappers for settings commands
│   │   └── logService.ts         # Frontend logging utility
│   ├── i18n/
│   │   └── index.ts              # i18next configuration
│   └── utils/
│       └── helper.ts             # Utility functions
│
├── public/
│   └── locales/                  # Frontend i18n JSON files
│       ├── en.json
│       └── zh.json
│
├── src-tauri/                    # Backend (Rust)
│   ├── src/
│   │   ├── main.rs               # Tauri app entry, AppState initialization
│   │   ├── lib.rs                # Module declarations, i18n init
│   │   ├── commands/
│   │   │   ├── mod.rs            # AppState struct, module re-exports
│   │   │   ├── printer.rs        # Tauri commands: get_printers, share_printer, etc.
│   │   │   ├── settings.rs       # Tauri commands: get_settings, set_close_to_tray, set_autostart
│   │   │   └── system.rs         # Tauri commands: set_language (persists + refreshes tray menu)
│   │   ├── models/
│   │   │   ├── mod.rs            # Module exports
│   │   │   ├── printer.rs        # Printer struct, PrinterStatus enum
│   │   │   └── settings.rs       # AppSettings struct (language, close_to_tray, autostart)
│   │   └── services/
│   │       ├── mod.rs            # Service exports
│   │       ├── printer_detector.rs  # Windows printer detection (PowerShell/WMIC)
│   │       ├── airprint_server.rs   # Main coordinator (mDNS + IPP)
│   │       ├── mdns_broadcaster.rs  # mDNS service registration
│   │       ├── settings.rs          # SettingsStore (settings.json) + autostart registry Run key
│   │       ├── ipp/
│   │       │   ├── mod.rs        # IPP module exports
│   │       │   └── server.rs     # IPP protocol handler
│   │       └── print_job.rs      # Print job processing
│   │   ├── tray.rs               # (at src/ root) System tray icon, menu, close-to-tray support
│   ├── locales/                  # Backend i18n YAML files
│   │   ├── en.yml
│   │   └── zh.yml
│   ├── icons/                    # Application icons (multiple sizes)
│   ├── Cargo.toml                # Rust dependencies
│   └── tauri.conf.json           # Tauri configuration
│
├── scripts/                      # Diagnostic utilities
│   ├── diagnose_mdns.py
│   └── diagnose_network.ps1
│
├── index.html                    # Main HTML template
├── package.json                  # Node.js dependencies
├── vite.config.ts                # Vite configuration (port 1420)
├── tsconfig.json                 # TypeScript strict mode
└── tailwind.config.js            # Tailwind + DaisyUI themes
```

## Build and Development Commands

```bash
# Install dependencies
pnpm install

# Development with hot reload (opens Tauri window)
pnpm tauri dev

# Production build (creates installer)
pnpm tauri build

# Debug build (unoptimized, for testing)
pnpm tauri build --debug

# Frontend-only development (browser, no Rust backend)
pnpm dev

# Preview production build
pnpm preview
```

## Architecture Details

### Frontend-Backend Communication

The frontend uses Tauri's `invoke` function to call Rust commands:

```typescript
// Frontend (TypeScript)
import { invoke } from "@tauri-apps/api/core";
const printers = await invoke<Printer[]>("get_printers");
```

```rust
// Backend (Rust) - commands/printer.rs
#[tauri::command]
pub fn get_printers(state: State<AppState>) -> Result<Vec<Printer>, String> {
    let detector = state.detector.lock().map_err(|e| e.to_string())?;
    Ok(detector.detect())
}
```

### State Management

`AppState` is a Tauri-managed state containing:
- `detector: Mutex<PrinterDetector>` - System printer detection
- `server: Mutex<AirPrintServer>` - Shared printer management

Initialized in `main.rs` during Tauri setup.

### AirPrint Service Implementation

When sharing a printer, the system:

1. **Starts IPP Server** (port 631) - Handles print jobs via IPP Everywhere™ protocol
2. **Registers mDNS Services** - Broadcasts 3 service types with identical instance names
3. **Processes Print Jobs** - Receives PDF/JPEG, sends to Windows print queue

Key requirements for iOS compatibility:
- All mDNS services must use the **same instance name** (e.g., `air-PrinterName`)
- IPP `Get-Printer-Attributes` must include `ipp-features-supported = ipp-everywhere`
- Must register `_print._sub._ipp._tcp` subtype

### Internationalization

**Frontend (i18next):**
- Files: `public/locales/{en,zh}.json`
- Format: JSON with nested keys
- Interpolation: `{{variable}}`

**Backend (rust-i18n):**
- Files: `src-tauri/locales/{en,zh}.yml`
- Format: YAML with nested keys
- Interpolation: `%{variable}`
- Macro usage: `t!("key", var = value)`

Language sync: Frontend changes language → invokes `set_language` command → backend updates `rust_i18n::locale()`.

## Code Style Guidelines

### TypeScript
- Strict mode enabled (`strict: true`)
- No unused locals or parameters (`noUnusedLocals: true`)
- ES2020 target with DOM libraries
- Module resolution: `bundler`

### Rust
- Edition 2021
- Error handling: Return `Result<T, String>` for Tauri commands
- Logging: Use `println!` with translated strings via `t!` macro
- Platform-specific code uses `#[cfg(target_os = "windows")]`

### Naming Conventions
- TypeScript: camelCase for functions/variables, PascalCase for classes/interfaces
- Rust: snake_case for functions/variables, PascalCase for structs/enums
- Translation keys: `snake_case` with dot notation (e.g., `logs.detector_scanning`)

## Testing and Debugging

### Firewall Configuration (Windows)
Run as Administrator in PowerShell:

```powershell
# Allow mDNS inbound (UDP 5353)
netsh advfirewall firewall add rule name="mDNS AirPrint" dir=in action=allow protocol=udp localport=5353

# Allow IPP port (TCP 631)
netsh advfirewall firewall add rule name="IPP Server" dir=in action=allow protocol=tcp localport=631
```

### Diagnostic Scripts
- `scripts/diagnose_network.ps1` - Network diagnostics
- `scripts/diagnose_mdns.py` - mDNS service discovery check

### Diagnostics Log

- Persistent JSONL log at `%APPDATA%\com.airprinter.app\print-diagnostics.jsonl` (2 MiB rotation);
  written by `services/diagnostics.rs::record()` — request metadata and errors only, never document bytes or TLS keys.
- Startup records environment facts (version, exe path, all IPv4 interfaces) — key for "wrong NIC advertised" bugs.
- In-app: Logs view → "诊断日志" tab shows the tail; buttons copy the log or reveal the file in Explorer
  (commands `get_log_info` / `read_logs` / `open_log_folder` in `commands/logs.rs`).
- First connectivity check: if a failing phone produces no `[IPP transport] peer=...` lines, the
  connection never reached the app (firewall/NIC/network), not an IPP problem.

### Reconcile Debounce (airprint_server.rs)

The 30s/300s `reconcile()` loop flapping guards:
- Offline withdrawal requires 2 consecutive bad polls (`OFFLINE_WITHDRAW_POLLS`) — sleeping USB printers must not flap the announcement.
- color/duplex re-announce requires 2 consecutive identical readings (`CAPABILITY_CONFIRM_POLLS`); a poll matching the shared profile clears the staged change.
- Failed auto-restore retries back off 30s→600s (`retry_delay`) and emit a `share-errors` event to the frontend log panel.

### Common Issues

1. **Discovery App finds printer, iOS doesn't:**
   - Check `_print._sub._ipp._tcp` is registered
   - Verify `ipp-features-supported = ipp-everywhere` in IPP response
   - Ensure all mDNS services have identical instance names

2. **Phone can't discover at all:**
   - Verify same Wi-Fi network
   - Check Windows firewall (UDP 5353, TCP 631)
   - Disable router AP isolation
   - Avoid 169.254.x.x link-local addresses

3. **Can discover but can't print:**
   - Check IPP server listening on `0.0.0.0:631`
   - Verify Windows printer driver works
   - Check logs for IPP request handling errors

## Security Considerations

- Application runs a local HTTP server on port 631
- mDNS broadcasts printer information on local network
- Temporary print files are created in system temp directory (auto-cleaned)
- Tauri CSP is set to `null` (development configuration)
- No authentication on IPP endpoint (designed for trusted local networks)

## Development Environment

### Recommended VS Code Extensions
- `tauri-apps.tauri-vscode` - Tauri support
- `rust-lang.rust-analyzer` - Rust language server

### Package Manager
- **pnpm** ( evidenced by `pnpm-lock.yaml` )
- Rust: crates.io via Tsinghua mirror (configured in `Cargo.toml`)

## Deployment

The application is built as a Windows desktop installer using Tauri's bundler. Output targets:
- `.msi` installer
- `.exe` standalone

Icons are provided in multiple formats in `src-tauri/icons/` for different use cases.

## License

See `LICENSE` file in project root.
