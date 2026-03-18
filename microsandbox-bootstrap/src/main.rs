//! microsandbox-bootstrap — PID 1 init process for Windows-hosted Linux VMs.
//!
//! Runs as the init process (kernel `init=/bootstrap` parameter) inside an HCS
//! compute system. Mounts SCSI-attached VHD layers, reads the sandbox spec from
//! the patch VHD, assembles an overlayfs root filesystem, and executes the user's
//! command.
//!
//! ## Boot sequence
//!
//! 1. Mount essential filesystems (`/proc`, `/sys`, `/dev`, `/tmp`)
//! 2. Parse kernel command line for `layers=N`
//! 3. Wait for SCSI devices to appear
//! 4. Mount layer VHDs read-only at `/mnt/layers/{0..N-1}`
//! 5. Mount patch VHD read-only at `/mnt/patch`
//! 6. Create tmpfs scratch at `/mnt/scratch` (overlayfs upper + work)
//! 7. Read `sandbox-spec.json` from patch VHD
//! 8. Assemble overlayfs at `/mnt/merged`
//! 9. Copy portal binary into merged root
//! 10. `pivot_root` into `/mnt/merged`
//! 11. Start portal daemon (port 4444)
//! 12. Exec user command with env/workdir from spec
//! 13. Forward exit code, power off VM

mod spec;

use std::ffi::CString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use spec::SandboxSpec;

//--------------------------------------------------------------------------------------------------
// Vsock constants for 9p host directory sharing
//--------------------------------------------------------------------------------------------------

/// Linux address family for vsock.
const AF_VSOCK: libc::c_int = 40;

/// CID for the host (Hyper-V host).
const VMADDR_CID_HOST: u32 = 2;

/// Base vsock port for 9p mount shares (must match Go hcs.BaseMountPort).
const P9_BASE_PORT: u32 = 50000;

/// Number of vsock connection attempts before giving up.
const VSOCK_CONNECT_RETRIES: u64 = 5;

/// Linux sockaddr_vm structure for vsock connections.
/// Layout matches the kernel's `struct sockaddr_vm` exactly.
#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_zero: [u8; 4],
}

//--------------------------------------------------------------------------------------------------
// Entry point
//--------------------------------------------------------------------------------------------------

fn main() {
    if let Err(e) = run() {
        eprintln!("bootstrap: fatal: {}", e);
        power_off();
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("bootstrap: starting");

    // 1. Mount essential filesystems.
    //    The rootfs ramdisk is read-only (ext4 from tar2ext4), so we mount
    //    tmpfs at /mnt to get a writable workspace for layer mounts/overlayfs.
    mount_essential_filesystems()?;
    mount_fs("tmpfs", "tmpfs", "/mnt", 0, "size=16m")?;

    // 2. Parse kernel command line to get layer count and mount count.
    let cmdline = fs::read_to_string("/proc/cmdline")?;
    let num_layers = parse_cmdline_usize(&cmdline, "layers")?;
    let num_mounts = parse_cmdline_usize(&cmdline, "mounts").unwrap_or(0);
    eprintln!("bootstrap: {} layer(s), {} mount(s) from cmdline", num_layers, num_mounts);

    // 3. Wait for SCSI devices (layers + patch + scratch).
    //    Mounts use 9p over vsock, not SCSI.
    let total_scsi = num_layers + 2;
    wait_for_scsi_devices(total_scsi)?;

    // 4. Mount layer VHDs read-only.
    for i in 0..num_layers {
        let dev = scsi_device_path(i);
        let mnt = format!("/mnt/layers/{}", i);
        fs::create_dir_all(&mnt)?;
        mount_fs("ext4", &dev, &mnt, libc::MS_RDONLY, "")?;
        eprintln!("bootstrap: layer {} mounted ({} -> {})", i, dev, mnt);
    }

    // 5. Mount patch VHD read-only.
    let patch_dev = scsi_device_path(num_layers);
    fs::create_dir_all("/mnt/patch")?;
    mount_fs("ext4", &patch_dev, "/mnt/patch", libc::MS_RDONLY, "")?;
    eprintln!("bootstrap: patch mounted ({})", patch_dev);

    // 6. Create tmpfs scratch for overlayfs upper/work.
    fs::create_dir_all("/mnt/scratch")?;
    mount_fs("tmpfs", "tmpfs", "/mnt/scratch", 0, "size=128m")?;
    fs::create_dir_all("/mnt/scratch/upper")?;
    fs::create_dir_all("/mnt/scratch/work")?;

    // 7. Read sandbox spec from patch VHD.
    let spec: SandboxSpec = {
        let data = fs::read_to_string("/mnt/patch/sandbox-spec.json")?;
        serde_json::from_str(&data)?
    };
    eprintln!(
        "bootstrap: spec loaded (sandbox_id={}, exec={})",
        spec.sandbox_id, spec.exec_path
    );

    // 8. Assemble overlayfs.
    //    lowerdir order: patch (highest priority) then layers top-to-bottom.
    let mut lower_parts = vec!["/mnt/patch".to_string()];
    for i in (0..num_layers).rev() {
        lower_parts.push(format!("/mnt/layers/{}", i));
    }
    let lowerdir = lower_parts.join(":");

    fs::create_dir_all("/mnt/merged")?;
    let overlay_opts = format!(
        "lowerdir={},upperdir=/mnt/scratch/upper,workdir=/mnt/scratch/work",
        lowerdir
    );
    mount_fs("overlay", "overlay", "/mnt/merged", 0, &overlay_opts)?;
    eprintln!("bootstrap: overlayfs assembled");

    // 9. Copy portal binary into the merged root (from the initial rootfs).
    if Path::new("/portal").exists() {
        let dst_dir = "/mnt/merged/usr/local/bin";
        let _ = fs::create_dir_all(dst_dir);
        fs::copy("/portal", format!("{}/portal", dst_dir))?;
        // Ensure executable
        set_executable(&format!("{}/portal", dst_dir));
        eprintln!("bootstrap: portal binary copied to merged root");
    }

    // 10. Prepare and execute pivot_root.
    fs::create_dir_all("/mnt/merged/proc")?;
    fs::create_dir_all("/mnt/merged/sys")?;
    fs::create_dir_all("/mnt/merged/dev")?;
    fs::create_dir_all("/mnt/merged/tmp")?;
    fs::create_dir_all("/mnt/merged/old_root")?;

    // pivot_root requires CWD to be on the new root.
    std::env::set_current_dir("/mnt/merged")?;
    pivot_root(".", "old_root")?;
    std::env::set_current_dir("/")?;

    // Mount essential filesystems in the new root.
    mount_fs("proc", "proc", "/proc", 0, "")?;
    mount_fs("sysfs", "sysfs", "/sys", 0, "")?;
    mount_fs("devtmpfs", "devtmpfs", "/dev", 0, "")?;
    mount_fs("tmpfs", "tmpfs", "/tmp", 0, "")?;
    let _ = fs::create_dir_all("/dev/pts");
    let _ = fs::create_dir_all("/dev/shm");
    let _ = mount_fs("devpts", "devpts", "/dev/pts", 0, "");
    let _ = mount_fs("tmpfs", "tmpfs", "/dev/shm", 0, "");

    // Detach old root.
    umount2("/old_root", libc::MNT_DETACH)?;
    let _ = fs::remove_dir("/old_root");

    eprintln!("bootstrap: pivot_root complete");

    // 10b. Mount host directories via 9p over vsock.
    for (i, mount) in spec.mounts.iter().enumerate() {
        let port = P9_BASE_PORT + i as u32;
        let guest_path = &mount.guest_path;
        if let Err(e) = fs::create_dir_all(guest_path) {
            eprintln!("bootstrap: WARNING: could not create mount point {}: {}", guest_path, e);
        }

        match mount_9p_vsock(port, guest_path, mount.readonly) {
            Ok(()) => eprintln!("bootstrap: mounted 9p at {} (vsock port {})", guest_path, port),
            Err(e) => eprintln!(
                "bootstrap: WARNING: 9p mount at {} failed: {}",
                guest_path, e
            ),
        }
    }

    // 11. Start portal daemon.
    let portal_path = "/usr/local/bin/portal";
    let mut _portal_child = None;
    if Path::new(portal_path).exists() {
        match Command::new(portal_path).arg("--port").arg("4444").spawn() {
            Ok(child) => {
                eprintln!("bootstrap: portal started (pid {})", child.id());
                _portal_child = Some(child);
            }
            Err(e) => eprintln!("bootstrap: portal start failed: {}", e),
        }
    }

    // 12. Execute user command.
    //     Resolve the program and arguments from the spec:
    //     - If exec_args is provided, use exec_path + exec_args directly.
    //     - If exec_path has spaces and no args, split into program + args.
    //     - If the program is not an absolute path, wrap in `/bin/sh -c` so the
    //       shell can resolve it via PATH (the bootstrap has no PATH as PID 1).
    let (program, extra_args): (String, Vec<String>) = if !spec.exec_args.is_empty() {
        // Args explicitly provided — use as-is.
        (spec.exec_path.clone(), spec.exec_args.clone())
    } else if spec.exec_path.contains(' ') {
        // No args but exec_path has spaces — split into program + args.
        let mut parts = spec.exec_path.splitn(2, ' ');
        let prog = parts.next().unwrap().to_string();
        let rest = parts.next().unwrap_or("");
        if !prog.starts_with('/') {
            // Non-absolute: wrap entire string in shell.
            ("/bin/sh".to_string(), vec!["-c".to_string(), spec.exec_path.clone()])
        } else {
            // Absolute path with embedded args — split them.
            let args: Vec<String> = rest.split_whitespace().map(|s| s.to_string()).collect();
            (prog, args)
        }
    } else if !spec.exec_path.starts_with('/') {
        // Single-word non-absolute command (e.g. "ls") — wrap in shell.
        ("/bin/sh".to_string(), vec!["-c".to_string(), spec.exec_path.clone()])
    } else {
        // Absolute path, no args — exec directly.
        (spec.exec_path.clone(), vec![])
    };

    eprintln!("bootstrap: exec {} {:?}", program, extra_args);

    let mut cmd = Command::new(&program);
    cmd.args(&extra_args);

    // Set environment variables.
    for env_str in &spec.env {
        if let Some((key, value)) = env_str.split_once('=') {
            cmd.env(key, value);
        }
    }

    // Set working directory.
    if let Some(ref workdir) = spec.workdir {
        cmd.current_dir(workdir);
    }

    let exit_code = match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("bootstrap: exec failed: {}", e);
            127
        }
    };

    eprintln!("bootstrap: command exited with code {}", exit_code);

    // 13. Clean up and power off.
    if let Some(mut child) = _portal_child {
        let _ = child.kill();
        let _ = child.wait();
    }

    // Flush filesystems and power off the VM cleanly.
    unsafe { libc::sync() };
    power_off();
    unreachable!()
}

//--------------------------------------------------------------------------------------------------
// Filesystem helpers
//--------------------------------------------------------------------------------------------------

/// Mount essential pseudo-filesystems on the initial root.
/// Ignores EBUSY (already mounted) since the kernel may have mounted some.
fn mount_essential_filesystems() -> Result<(), Box<dyn std::error::Error>> {
    for dir in &["/proc", "/sys", "/dev", "/tmp", "/mnt"] {
        let _ = fs::create_dir_all(dir);
    }
    let _ = mount_fs("proc", "proc", "/proc", 0, "");
    let _ = mount_fs("sysfs", "sysfs", "/sys", 0, "");
    let _ = mount_fs("devtmpfs", "devtmpfs", "/dev", 0, "");
    let _ = mount_fs("tmpfs", "tmpfs", "/tmp", 0, "");
    Ok(())
}

/// Mount a filesystem via the `mount(2)` syscall.
fn mount_fs(
    fstype: &str,
    source: &str,
    target: &str,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    let source_c = c_string(source)?;
    let target_c = c_string(target)?;
    let fstype_c = c_string(fstype)?;
    let data_c = c_string(data)?;

    let data_ptr = if data.is_empty() {
        std::ptr::null()
    } else {
        data_c.as_ptr() as *const libc::c_void
    };

    let ret = unsafe {
        libc::mount(
            source_c.as_ptr(),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            flags,
            data_ptr,
        )
    };

    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Unmount a filesystem with flags.
fn umount2(target: &str, flags: libc::c_int) -> io::Result<()> {
    let target_c = c_string(target)?;
    let ret = unsafe { libc::umount2(target_c.as_ptr(), flags) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Change the root filesystem via `pivot_root(2)`.
fn pivot_root(new_root: &str, put_old: &str) -> io::Result<()> {
    let new_root_c = c_string(new_root)?;
    let put_old_c = c_string(put_old)?;
    let ret = unsafe {
        libc::syscall(
            libc::SYS_pivot_root,
            new_root_c.as_ptr(),
            put_old_c.as_ptr(),
        )
    };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Set the executable bit on a file.
fn set_executable(path: &str) {
    let path_c = match c_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    unsafe { libc::chmod(path_c.as_ptr(), 0o755) };
}

/// Power off the VM cleanly.
fn power_off() -> ! {
    unsafe {
        libc::sync();
        libc::reboot(libc::LINUX_REBOOT_CMD_POWER_OFF);
    }
    // If reboot fails, loop forever (kernel will eventually kill us).
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

//--------------------------------------------------------------------------------------------------
// Vsock + 9p mount helpers
//--------------------------------------------------------------------------------------------------

/// Connect to the host via vsock at the given port.
/// Retries with exponential backoff since the p9 server may still be starting.
fn vsock_connect(port: u32) -> io::Result<i32> {
    let mut last_err = io::Error::new(io::ErrorKind::Other, "no attempts made");
    for attempt in 1..=VSOCK_CONNECT_RETRIES {
        let fd = unsafe { libc::socket(AF_VSOCK, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let addr = SockaddrVm {
            svm_family: AF_VSOCK as u16,
            svm_reserved1: 0,
            svm_port: port,
            svm_cid: VMADDR_CID_HOST,
            svm_zero: [0; 4],
        };

        let ret = unsafe {
            libc::connect(
                fd,
                &addr as *const SockaddrVm as *const libc::sockaddr,
                std::mem::size_of::<SockaddrVm>() as libc::socklen_t,
            )
        };

        if ret == 0 {
            return Ok(fd);
        }

        last_err = io::Error::last_os_error();
        unsafe { libc::close(fd) };

        if attempt < VSOCK_CONNECT_RETRIES {
            thread::sleep(Duration::from_millis(100 * attempt));
        }
    }
    Err(last_err)
}

/// Mount a host directory via 9p over vsock.
fn mount_9p_vsock(port: u32, guest_path: &str, readonly: bool) -> io::Result<()> {
    let fd = vsock_connect(port)?;
    let opts = format!(
        "trans=fd,rfdno={},wfdno={},version=9p2000.L,msize=262144",
        fd, fd
    );
    let mut flags: libc::c_ulong = libc::MS_NODEV | libc::MS_NOSUID;
    if readonly {
        flags |= libc::MS_RDONLY;
    }
    mount_fs("9p", "hostmount", guest_path, flags, &opts)?;
    // Do NOT close fd — kernel takes ownership for the mount.
    Ok(())
}

//--------------------------------------------------------------------------------------------------
// SCSI device helpers
//--------------------------------------------------------------------------------------------------

/// Returns the Linux device path for a SCSI LUN.
///
/// LUN 0 -> `/dev/sda`, LUN 1 -> `/dev/sdb`, etc.
fn scsi_device_path(lun: usize) -> String {
    if lun < 26 {
        format!("/dev/sd{}", (b'a' + lun as u8) as char)
    } else {
        // For > 26 devices: sda..sdz, sdaa..sdaz, etc.
        // Unlikely for our use case, but handle gracefully.
        let first = (b'a' + (lun / 26 - 1) as u8) as char;
        let second = (b'a' + (lun % 26) as u8) as char;
        format!("/dev/sd{}{}", first, second)
    }
}

/// Wait for all expected SCSI devices to appear in `/dev`.
fn wait_for_scsi_devices(count: usize) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = Duration::from_secs(10);
    let poll = Duration::from_millis(50);
    let start = Instant::now();

    for i in 0..count {
        let dev = scsi_device_path(i);
        while !Path::new(&dev).exists() {
            if start.elapsed() > timeout {
                return Err(format!("timeout waiting for SCSI device {}", dev).into());
            }
            thread::sleep(poll);
        }
    }

    eprintln!("bootstrap: all {} SCSI device(s) ready", count);
    Ok(())
}

//--------------------------------------------------------------------------------------------------
// Kernel command line parsing
//--------------------------------------------------------------------------------------------------

/// Parse a `key=value` parameter from a kernel command line string.
fn parse_cmdline_usize(cmdline: &str, key: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let prefix = format!("{}=", key);
    for param in cmdline.split_whitespace() {
        if let Some(value) = param.strip_prefix(&prefix) {
            return Ok(value.parse::<usize>()?);
        }
    }
    Err(format!("'{}=N' not found in /proc/cmdline", key).into())
}

//--------------------------------------------------------------------------------------------------
// Utility
//--------------------------------------------------------------------------------------------------

/// Create a CString, mapping NulError to io::Error.
fn c_string(s: &str) -> io::Result<CString> {
    CString::new(s).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}
