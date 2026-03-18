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
- [x] VM output streams to host terminal in real time
- [x] Clean VM shutdown (bootstrap power_off → HCS teardown)
- [x] Hyper-V detection without admin privileges (WMI `HypervisorPresent`)
- [x] VHD permission grants with retry and backslash normalization (`icacls`)
- [x] Cross-compilation of bootstrap (453KB static musl ELF via `cargo-zigbuild`)
- [x] Cross-compilation of portal (2.6MB static musl ELF via `cargo-zigbuild`)
- [x] Server lifecycle smoke tests pass (4/4)
- [x] All unit tests pass (232 total: 152 Rust + 20 Go + 4 smoke)

## Blockers for Production

### P0 — Must fix before any user can use it

- [x] **HCN endpoint creation fails** — Fixed: all HCN APIs (`HcnCreateNetwork`, `HcnOpenNetwork`, `HcnCreateEndpoint`, etc.) expect `REFGUID` (binary 16-byte struct pointer), not UTF-16 string pointers. Switched to `go-winio/pkg/guid` for proper binary GUID handling. Endpoint creation now succeeds.
  - File: `microsandbox-hcs-worker/internal/hcn/network.go`
  - Note: HCN load balancer fails separately — `VIP` field is unknown on Win11 26200, and ELB policy rejected on NAT network. This is a P2 port-forwarding issue, not a GUID issue.

- [x] **Exec path not split into program + args** — Fixed: `resolve_sandbox_spec` now wraps non-absolute exec paths in `/bin/sh -c "..."` on Windows (cfg-gated). Absolute paths with embedded args (e.g. `/bin/echo hello`) are split into program + argv. Bootstrap also updated with equivalent safety-net logic.
  - Files: `microsandbox-core/lib/management/sandbox.rs` (resolve_sandbox_spec), `microsandbox-bootstrap/src/main.rs`

- [ ] **Named pipe conflicts on restart** — running `msb exe` twice quickly hits "Access denied" on `\\.\pipe\microsandbox-tmp` because the previous worker's pipe wasn't cleaned up. The 500ms retry is insufficient.
  - File: `microsandbox-hcs-worker/internal/pipe/listener.go`
  - Impact: second run fails until the pipe is garbage-collected by Windows

### P1 — Required for `msb shell` interactive use

- [ ] **Interactive terminal (stdin relay)** — the console pipe relay works for output but stdin from the host terminal doesn't reach the guest shell interactively. Need to set the host terminal to raw mode and handle Ctrl+C/resize signals.
  - Files: `microsandbox-hcs-worker/main.go` (relayConsole), `microsandbox-core/lib/management/sandbox.rs` (run)
  - Impact: `msb shell dev` shows output but keyboard input doesn't work

- [ ] **Plan9 / VirtualSmb host directory sharing** — Plan9 shares cause HCS `Construct` errors on Windows 11 build 26200. VirtualSmb is the fallback but not implemented.
  - File: `microsandbox-core/lib/backend/windows/hcs.rs` (currently disabled)
  - Impact: `volumes: [".:/workspace"]` in Sandboxfile does nothing

### P2 — Required for server API and multi-sandbox

- [ ] **Admin privilege detection** — HCN operations require elevation but the error is a cryptic HRESULT. Should detect non-admin early and show "Run as Administrator" message.
  - File: `microsandbox-core/lib/backend/windows/hcs.rs`

- [ ] **HCN NAT network reuse** — `enumerateNetworks` finds the existing network ID but `getNetworkName` fails to query its properties, so `EnsureNATNetwork` tries to create a duplicate and fails. The fallback re-enumerate also fails to match by name.
  - File: `microsandbox-hcs-worker/internal/hcn/network.go`

- [ ] **Server API portal proxy** — the server handler constructs `http://127.0.0.1:<port>` to reach the portal, but without working HCN load balancers, the port forwarding doesn't exist. Depends on P0 networking fix.
  - File: `microsandbox-server/lib/handler.rs`

### P3 — Polish and distribution

- [ ] **Automated boot bundle build** — `scripts/build_boot_bundle.sh` works in Linux/WSL but the actual build on Windows requires manual `cargo-zigbuild` + Python tarfile + tar2ext4 steps. Need a `make build_bundle_windows` target or PowerShell equivalent.
  - Workaround: `cargo-zigbuild` + Python tarfile script (documented below)

- [ ] **rootfs.vhd file permissions** — tar archives created on Windows don't preserve Unix executable bits. Currently using a Python tarfile script to set `mode=0o755`. Should be automated in the build pipeline.

- [ ] **Boot bundle auto-download** — `boot_bundle.rs` `ensure_bundle()` currently returns an error with instructions. Should auto-download from GitHub releases.
  - File: `microsandbox-core/lib/backend/windows/boot_bundle.rs`

- [ ] **GitHub Actions Windows release** — need CI job to build `msb.exe`, `msbserver.exe`, `msbrun-hcs.exe`, and the boot bundle zip.
  - File: `.github/workflows/release.yml`

- [ ] **`msb run` / `msb exec` CLI parity** — `msb exec` needs to connect to the portal on a running sandbox and send JSON-RPC. Depends on networking.

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
│  ├─ HCN network │                    │  │  Alpine rootfs     │
│  ├─ HCS VM      │   SCSI VHDs       │  │  (overlayfs)       │
│  └─ pipe server │───────────────────┼──┤   ├─ /bin, /usr    │
│                 │                    │  │   ├─ /etc          │
│ Boot Bundle     │                    │  │   └─ /workspace    │
│  ├─ kernel      │                    │  │                    │
│  ├─ rootfs.vhd  │                    └──┴──────────────────┘
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
msb exe alpine -e "/bin/ls"
msb exe alpine -e "/bin/echo hello from microsandbox"
```
