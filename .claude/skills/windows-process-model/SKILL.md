---
name: windows-process-model
description: Windows process lifecycle — Job Objects, named pipe IPC, ConPTY, CreateProcessW, handle inheritance, msbrun-hcs.exe worker contract
user-invocable: false
---

# Windows Process Model

## Fixed decisions

1. **Job Objects** replace Unix signal trees for process lifetime management.
2. **ConPTY** replaces Unix PTY for interactive terminal attach.
3. **Named pipes** replace Unix domain sockets for control IPC.
4. **CreateProcessW** with `EXTENDED_STARTUPINFO_PRESENT` replaces `fork`/`exec`.
5. **`msbrun-hcs.exe`** is the per-sandbox runtime worker (replaces `msbrun supervisor` + `msbrun microvm`).

## Current Unix process model — what it replaces

`microsandbox-utils/lib/runtime/supervisor.rs` lines 105-148:

```rust
// Unix PTY creation
let pty = openpty(None, None)?;
let flags = OFlag::from_bits_truncate(fcntl(&pty.master, FcntlArg::F_GETFL)?);
fcntl(&pty.master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;

// Session setup in child
command.pre_exec(|| {
    libc::setsid();
    libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 1 as libc::c_long);
    Ok(())
});

// Async I/O on master
let master_read = AsyncFd::new(master_read_file)?;
```

Signal handling (lines 191-192):

```rust
let mut sigterm = signal(SignalKind::terminate())?;
let mut sigint = signal(SignalKind::interrupt())?;
```

Process termination (lines 232, 248):

```rust
nix::sys::signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
```

TTY raw mode in `monitor.rs` lines 250-260:

```rust
let term = nix::sys::termios::tcgetattr(BorrowedFd::borrow_raw(libc::STDIN_FILENO))?;
let mut raw_term = term.clone();
nix::sys::termios::cfmakeraw(&mut raw_term);
nix::sys::termios::tcsetattr(fd, SetArg::TCSANOW, &raw_term)?;
```

Server process control in `management.rs`:

```rust
// Liveness check (line 96)
let process_running = unsafe { libc::kill(pid, 0) == 0 };

// Detach (lines 206-210)
command.pre_exec(|| { libc::setsid(); Ok(()) });

// Terminate (lines 372-395)
signal::kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
```

## Job Objects

Replace Unix signal-tree semantics for process lifetime:

```rust
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;

unsafe {
    // Create job object
    let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());

    // Configure: kill all processes when job handle closes
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    SetInformationJobObject(
        job,
        JobObjectExtendedLimitInformation,
        &info as *const _ as *const _,
        std::mem::size_of_val(&info) as u32,
    );

    // Assign worker process to job
    AssignProcessToJobObject(job, worker_process_handle);
}
```

`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` = `0x2000` — ensures child processes die if the parent crashes.

Higher-level crate: `win32job` provides safe wrappers.

## Named pipe control endpoint

Each runtime worker listens on:

```text
\\.\pipe\microsandbox-runtime\<runtime-id>
```

Using Tokio named pipes:

```rust
use tokio::net::windows::named_pipe::ServerOptions;

let server = ServerOptions::new()
    .first_pipe_instance(true)
    .create(r"\\.\pipe\microsandbox-runtime\abc123")?;

server.connect().await?;

// Read/write JSON commands
let mut buf = [0u8; 4096];
let n = server.read(&mut buf).await?;
let cmd: ControlCommand = serde_json::from_slice(&buf[..n])?;
```

Supported commands:

```rust
enum ControlCommand {
    Status,
    Stop,
    Attach { interactive: bool },
    CollectLogs,
}

enum ControlResponse {
    Status { running: bool, uptime_secs: u64 },
    Stopped,
    Attached { /* ConPTY handle info */ },
    Logs { data: Vec<u8> },
    Error { message: String },
}
```

## ConPTY

Replaces Unix PTY for interactive terminal sessions:

```rust
use windows_sys::Win32::System::Console::*;
use windows_sys::Win32::System::Pipes::*;

unsafe {
    // Create pipe pairs for ConPTY
    let mut pty_input_read = INVALID_HANDLE_VALUE;
    let mut pty_input_write = INVALID_HANDLE_VALUE;
    let mut pty_output_read = INVALID_HANDLE_VALUE;
    let mut pty_output_write = INVALID_HANDLE_VALUE;

    CreatePipe(&mut pty_input_read, &mut pty_input_write, std::ptr::null(), 0);
    CreatePipe(&mut pty_output_read, &mut pty_output_write, std::ptr::null(), 0);

    // Create pseudo console
    let size = COORD { X: 80, Y: 24 };
    let mut hpc: HPCON = 0;
    CreatePseudoConsole(size, pty_input_read, pty_output_write, 0, &mut hpc);

    // Attach to process via PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
    // in STARTUPINFOEXW
}
```

ConPTY workflow:
1. `CreatePipe` x2 — input pipe and output pipe.
2. `CreatePseudoConsole(size, hInput, hOutput, flags, &hPC)`.
3. Initialize `STARTUPINFOEXW` with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`.
4. `CreateProcessW` with `EXTENDED_STARTUPINFO_PRESENT`.
5. Read from output pipe, write to input pipe for terminal I/O.
6. `ResizePseudoConsole` for terminal resize.
7. `ClosePseudoConsole` on cleanup.

Higher-level crate: `winpty-rs` provides safe ConPTY wrappers.

## Process creation

```rust
use windows_sys::Win32::System::Threading::*;

unsafe {
    let mut si: STARTUPINFOEXW = std::mem::zeroed();
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;

    // Initialize thread attribute list for ConPTY
    let mut attr_list_size = 0;
    InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_list_size);
    // ... allocate and initialize

    UpdateProcThreadAttribute(
        si.lpAttributeList,
        0,
        PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
        hpc as *mut _,
        std::mem::size_of::<HPCON>(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );

    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    CreateProcessW(
        exe_path.as_ptr(),
        cmd_line.as_mut_ptr(),
        std::ptr::null(),       // process security
        std::ptr::null(),       // thread security
        0,                       // don't inherit handles
        EXTENDED_STARTUPINFO_PRESENT | CREATE_NEW_PROCESS_GROUP,
        std::ptr::null(),       // environment
        work_dir.as_ptr(),
        &si.StartupInfo,
        &mut pi,
    );
}
```

## Tokio signal handling on Windows

| Unix | Windows |
|---|---|
| `signal::unix::signal(SignalKind::terminate())` | Not available |
| `signal::unix::signal(SignalKind::interrupt())` | `tokio::signal::ctrl_c()` |
| N/A | `tokio::signal::windows::ctrl_break()` |
| N/A | `tokio::signal::windows::ctrl_close()` |

```rust
#[cfg(windows)]
async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut ctrl_break = tokio::signal::windows::ctrl_break().unwrap();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("received Ctrl+C"); }
        _ = ctrl_break.recv() => { tracing::info!("received Ctrl+Break"); }
    }
}
```

## msbrun-hcs.exe worker contract

**Input:** Receives `RuntimeSpawnRequest` via stdin or named pipe argument:

```rust
struct RuntimeSpawnRequest {
    runtime_id: String,
    sandbox_spec: ResolvedSandboxSpec,
    materialized_rootfs: WindowsMaterializedRootfs,
    control_pipe: String,  // \\.\pipe\microsandbox-runtime\<id>
}
```

**Lifecycle:**
1. Parse input.
2. Create Job Object and assign self.
3. Create HCN endpoint + load balancers.
4. Create HCS compute system with VHDs + Plan9 shares.
5. Start compute system.
6. Open control named pipe and listen for commands.
7. Wait for VM exit or stop command.
8. Tear down: close HCS, delete HCN endpoint + LBs, delete scratch VHDX.
9. Exit.

**Cleanup guarantee:** Job Object with `KILL_ON_JOB_CLOSE` ensures all child processes die if the worker crashes.

## Anti-patterns

- **Don't emulate Unix signals on Windows.** Use Job Objects + named pipe commands.
- **Don't use `AsyncFd` on Windows.** It's Unix-only. Use `tokio::net::windows::named_pipe` or `tokio::io`.
- **Don't use `libc::kill` on Windows.** Use `TerminateProcess` or `OpenProcess` + `WaitForSingleObject`.
- **Don't use `setsid` on Windows.** Use `CREATE_NEW_PROCESS_GROUP` flag.
- **Don't use `openpty` on Windows.** Use ConPTY (`CreatePseudoConsole`).
- **Don't share ConPTY handles across processes.** Each attach session should create its own ConPTY.
- **Don't poll for process exit.** Use `WaitForSingleObject(process_handle, INFINITE)` or tokio's process await.
