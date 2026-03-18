# Windows Support Checklist

Status of the Windows VM pipeline using Hyper-V (HCS/HCN) — no Docker dependency.

## Verified Working (E2E tested on Windows 11 26200)

- [x] OCI image pull from Docker Hub (alpine:latest)
- [x] Layer tarball → ext4 VHD conversion via tar2ext4
- [x] Patch VHD generation with `sandbox-spec.json`
- [x] Scratch VHDX creation (PowerShell `New-VHD`)
- [x] Boot bundle loading and validation (`kernel`, `rootfs.vhd`, `bootstrap`, `portal`, `tar2ext4.exe`, `manifest.json`)
- [x] HCS compute system creation with SCSI VHD attachments
- [x] Linux kernel boot inside Hyper-V VM (WSL2 kernel, ~1s boot)
- [x] Kernel cmdline: `root=/dev/ram0 init=/bootstrap console=ttyS0 panic=1 layers=N`
- [x] Serial console I/O via COM port → named pipe relay (Go worker ↔ host stdin/stdout)
- [x] SCSI device discovery inside guest (layer VHDs, patch VHD, scratch VHDX)
- [x] Bootstrap init (PID 1): mounts layers, reads spec, assembles overlayfs, pivot_root
- [x] Portal binary starts inside guest on port 4444
- [x] User command execution inside overlayfs root (`/bin/echo`, `/bin/ls` verified)
- [x] VM output streams to host terminal in real time (ANSI colors rendered)
- [x] Clean VM shutdown (bootstrap power_off → HCS teardown)
- [x] Worker auto-exit on VM shutdown (console pipe EOF → context cancel → clean teardown)
- [x] Interactive terminal raw mode (disables echo/line-buffering, Ctrl+C flows to guest)
- [x] Back-to-back `msb exe` without pipe conflicts (session-scoped pipe names)
- [x] Hyper-V detection without admin privileges (WMI `HypervisorPresent`)
- [x] VHD permission grants with retry and backslash normalization (`icacls`)
- [x] Cross-compilation of bootstrap (469KB static musl ELF via `cargo-zigbuild`)
- [x] Cross-compilation of portal (2.6MB static musl ELF via `cargo-zigbuild`)
- [x] Server lifecycle smoke tests pass (4/4)
- [x] All unit tests pass (138 Rust + Go HCS/pipe tests + 4 smoke)
- [x] HCS V2 Plan9 schema structure corrected (nested `{ "Shares": [...] }` with Port field)
- [x] HCS V2 HvSocket schema: must be `Devices.HvSocket.HvSocketConfig.ServiceTable`, NOT `VirtualMachine.HvSockets`
- [x] Windows `-v` volume mount parsing: drive letter colons (`C:\`) no longer rejected as delimiters
- [x] Live bidirectional host directory sharing verified E2E (host→guest read, guest→host write)

## Blockers for Production

### P0 — Must fix before any user can use it

- [x] **HCN endpoint creation fails** — Fixed: all HCN APIs (`HcnCreateNetwork`, `HcnOpenNetwork`, `HcnCreateEndpoint`, etc.) expect `REFGUID` (binary 16-byte struct pointer), not UTF-16 string pointers. Switched to `go-winio/pkg/guid` for proper binary GUID handling. Endpoint creation now succeeds.
  - File: `microsandbox-hcs-worker/internal/hcn/network.go`
  - Note: HCN load balancer fails separately — `VIP` field is unknown on Win11 26200, and ELB policy rejected on NAT network. This is a P2 port-forwarding issue, not a GUID issue.

- [x] **Exec path not split into program + args** — Fixed: `resolve_sandbox_spec` now wraps non-absolute exec paths in `/bin/sh -c "..."` on Windows (cfg-gated). Absolute paths with embedded args (e.g. `/bin/echo hello`) are split into program + argv. Bootstrap also updated with equivalent safety-net logic.
  - Files: `microsandbox-core/lib/management/sandbox.rs` (resolve_sandbox_spec), `microsandbox-bootstrap/src/main.rs`

- [x] **Named pipe conflicts on restart** — Fixed: each invocation now generates a unique session ID (first 8 chars of a UUID) and uses it for session-scoped pipe names (`\\.\pipe\microsandbox-{key}-{session}`), console pipe names, and HCS compute system names. No two runs share a pipe name, so Windows TIME_WAIT on closed pipes no longer causes "Access denied".
  - File: `microsandbox-core/lib/backend/windows/hcs.rs` (`start()` method)
  - The Go worker receives the unique pipe path via `--pipe` — no worker-side changes needed.

### P1 — Required for `msb shell` interactive use

- [x] **Worker doesn't exit when VM shuts down** — Fixed: `relayConsole` now detects console pipe EOF (VM exit) via an `outputDone` channel and calls `cancel()` to unblock the main `select` loop. The worker performs HCS teardown, cleans up HCN networking, and exits cleanly. The Rust side's `tasklist` polling detects the worker exit and proceeds.
  - File: `microsandbox-hcs-worker/main.go` (`relayConsole` with `outputDone` channel → `cancel()`)
  - E2E verified: `Console pipe closed (VM exited), initiating shutdown` → `Cleanup complete, exiting` in ~1s

- [x] **Interactive terminal (stdin relay)** — Fixed: `enableRawMode()` disables `ENABLE_ECHO_INPUT`, `ENABLE_LINE_INPUT`, and `ENABLE_PROCESSED_INPUT` on the Windows console via `SetConsoleMode` (`golang.org/x/sys/windows`). Enables `ENABLE_VIRTUAL_TERMINAL_INPUT` for escape sequences and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout for ANSI color rendering. Terminal mode restored on exit via deferred restore function. Non-terminal stdin (piped) is handled gracefully.
  - File: `microsandbox-hcs-worker/main.go` (`enableRawMode`, `relayConsole`)
  - E2E verified: `/bin/ls /` renders ANSI colors correctly; raw keypresses flow to guest

- [x] **Host directory sharing (live 9p over vsock)** — Bidirectional, live file sharing using 9p2000.L over Hyper-V sockets. Go worker runs a p9 file server per mount (hugelgupf/p9 library), bootstrap connects via AF_VSOCK and mounts with `trans=fd`. Host writes are immediately visible in the guest and vice versa.
  - [x] SCSI VHD snapshot mounts (read-only and read-write, one-shot) — superseded by 9p
  - [x] Live bidirectional sharing via 9p2000.L over Hyper-V sockets (vsock)
  - Files: `p9server/server.go` (9p file server over HvSocket), `hcs.rs` (Plan9 shares from mounts, HvSocket schema), `bootstrap/main.rs` (vsock connect, 9p mount)
  - Note: HCS built-in Plan9 shares are still in the VM document but unused by the guest. The 9p transport goes through our own p9 server over HvSocket/vsock, not HCS's Plan9 provider.
  - Note: HCS on Win11 26200 rejects unknown fields in VM documents with a generic "invalid JSON" error that only names the unknown field, not its full path. Debugging requires comparing against the hcsshim schema types.

### P2 — Required for server API and multi-sandbox

- [x] **Admin privilege detection** — Implemented: Rust `check_admin_elevated()` at `hcs.rs:316-342` called at `start()` line 399; Go `checkElevated()` at `main.go:50-51` (defense-in-depth); test at `hcs.rs:910-926`.
  - Files: `microsandbox-core/lib/backend/windows/hcs.rs`, `microsandbox-hcs-worker/main.go`

- [x] **HCN NAT network reuse** — Fixed: `EnsureNATNetwork` rewritten with 4-phase delete-migrate strategy. Phase 1: fast-path reuse by deterministic GUID. Phase 2: find stale random-GUID networks by name (HCN enumeration + PowerShell `Get-HnsNetwork` fallback), delete to migrate. Phase 3: create with deterministic GUID. Phase 4: on conflict, force cleanup (HCN + PowerShell `Remove-HnsNetwork`) and retry. `deleteNetwork` wraps `HcnDeleteNetwork`. PowerShell fallback handles system networks (Default Switch, WSL) that refuse HCN queries.
  - File: `microsandbox-hcs-worker/internal/hcn/network.go`

- [x] **Server API portal proxy** — Resolves with P2-2: the portal relay chain (`hcs.rs` → Go worker TCP relay → guest:4444) was already wired correctly but required a working HCN NAT network for endpoint IP assignment. Added explicit error logging in `main.go` when portal is requested but networking is unavailable, preventing silent failures.
  - Files: `microsandbox-hcs-worker/main.go`, `microsandbox-server/lib/handler.rs`

### P3 — Polish and distribution

- [x] **Automated boot bundle build** — `scripts/Build-BootBundle.ps1` automates the full Windows build: cargo-zigbuild cross-compilation, Python tarfile with correct Unix permissions (mode=0o755, uid/gid=0), tar2ext4 VHD conversion, manifest generation with SHA256. Callable via `make build_bundle_windows`.
  - Files: `scripts/Build-BootBundle.ps1`, `Makefile` (new `build_bundle_windows` target)

- [x] **rootfs.vhd file permissions** — Handled by the embedded Python tarfile snippet in `Build-BootBundle.ps1`, which sets `mode=0o755`, `uid=0`, `gid=0` on all entries. Fully automated in the build pipeline.

- [x] **rootfs.vhd rebuild required for bootstrap changes** — Automated: `Build-BootBundle.ps1` cross-compiles bootstrap and portal via `cargo zigbuild --target x86_64-unknown-linux-musl`, rebuilds rootfs.vhd, and regenerates the manifest in a single invocation.

- [x] **Boot bundle auto-download** — `ensure_bundle()` now auto-downloads the versioned boot bundle zip from GitHub releases when not found locally. Downloads `.sha256` sidecar for integrity verification, streams the zip with `indicatif` progress bar (under `cli` feature), extracts via PowerShell `Expand-Archive`, and validates via `load_bundle()`.
  - File: `microsandbox-core/lib/backend/windows/boot_bundle.rs`

- [x] **GitHub Actions Windows release** — Added `windows-latest` / `x86_64-pc-windows-msvc` to the `build-and-package` CI matrix. Windows steps: setup Go 1.22, install cargo-zigbuild + zig + musl target, download kernel from `boot-assets` release tag, build tar2ext4.exe from hcsshim source, run `Package-WindowsRelease.ps1` to produce host binary zip and boot bundle zip with SHA256 checksums.
  - Files: `.github/workflows/release.yml`, `scripts/Package-WindowsRelease.ps1`

- [x] **`msb run` / `msb exec` CLI parity** — Already functionally complete. The CLI is platform-agnostic, `sandbox::run()` is cfg-gated for Windows via `WindowsHcsBackend`, and HCN networking is resolved. Verified via `scripts/ci/windows_e2e_test.ps1`.
  - File: `scripts/ci/windows_e2e_test.ps1`

## Architecture

```
Host (Windows)                          Guest (Linux VM)
┌─────────────────┐                    ┌──────────────────────┐
│ msb.exe (Rust)  │                    │ bootstrap (PID 1)    │
│  ├─ OCI pull    │                    │  ├─ mount layers     │
│  ├─ tar2ext4    │                    │  ├─ overlayfs        │
│  └─ spawn worker│                    │  ├─ pivot_root       │
│                 │   HCS + COM pipe   │  ├─ portal :4444     │
│ msbrun-hcs.exe ─┼───────────────────┼──┤  └─ exec command   │
│  (Go worker)    │   stdin/stdout     │  │                    │
│  ├─ HCN network │   (raw mode)      │  │  Alpine rootfs     │
│  ├─ HCS VM      │   SCSI VHDs       │  │  (overlayfs)       │
│  └─ pipe server │───────────────────┼──┤   ├─ /bin, /usr    │
│  └─ EOF detect  │                    │  │   ├─ /etc          │
│                 │                    │  │   └─ /workspace    │
│ Boot Bundle     │                    │  │                    │
│  ├─ kernel      │                    └──┴──────────────────┘
│  ├─ rootfs.vhd  │
│  ├─ bootstrap   │
│  ├─ portal      │
│  └─ tar2ext4.exe│
└─────────────────┘
```

## How to Build (Current Manual Process)

```powershell
# Prerequisites: Rust, Go, cargo-zigbuild, Python 3, tar2ext4.exe

# 1. Build host binaries
cargo build --release --bin msb --bin msbserver --features cli
cd microsandbox-hcs-worker && go build -buildvcs=false -o ../target/release/msbrun-hcs.exe .

# 2. Cross-compile guest binaries (Windows → Linux musl)
pip install ziglang && cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl
# Set PATH to include zig: $env:PATH += ";$env:APPDATA\Python\Python313\site-packages\ziglang"
cd microsandbox-bootstrap && cargo zigbuild --release --target x86_64-unknown-linux-musl
cd .. && cargo zigbuild --release --target x86_64-unknown-linux-musl -p microsandbox-portal --bin portal

# 3. Build rootfs.vhd (Python for correct Unix permissions in tar)
python3 build_rootfs.py  # creates tar with mode=0o755, then tar2ext4 -vhd

# 4. Assemble boot bundle at %LOCALAPPDATA%\Microsandbox\runtime\windows\uvm\0.2.6\
#    kernel, rootfs.vhd, bootstrap, portal, tar2ext4.exe, manifest.json
```

## How to Test

```powershell
# Unit tests
cargo test -p microsandbox-core --lib
cd microsandbox-hcs-worker && go test -buildvcs=false ./...

# Smoke test (no Hyper-V needed)
$env:MSB_BIN = ".\target\release\msb.exe"
powershell -File scripts/ci/windows_smoke_test.ps1

# E2E test (requires Hyper-V + boot bundle installed)
# Note: use MSYS_NO_PATHCONV=1 in Git Bash to prevent path mangling
msb exe alpine -e "/bin/echo hello from microsandbox"
msb exe alpine -e "/bin/ls /"

# Back-to-back test (verifies pipe cleanup)
msb exe alpine -e "/bin/echo run1" && msb exe alpine -e "/bin/echo run2"
```
