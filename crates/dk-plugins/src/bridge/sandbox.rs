// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sandbox a plugin host process enters before it loads any plugin.
//!
//! It guards against a plugin that misbehaves (or was made to): it may
//! read anything (samplers and convolution reverbs load files from
//! wherever they're kept), but it may only write where plugins keep their
//! own things, so it can't damage the user's files; it can't reach the
//! network, so it can't send them anywhere; and it can't debug, signal or
//! read the memory of other processes, or load kernel modules and the
//! like.
//!
//! - Landlock (the kernel's own sandboxing) limits writing, making files
//!   and connecting to Unix sockets to the folders in [`writable`] (and the
//!   sockets of the X server, the Wayland compositor and the system bus,
//!   which rtkit is on), and TCP to nothing; and it keeps signals within
//!   the sandbox. (Not abstract Unix sockets: X clients reach the X server
//!   through one first, and libxcb doesn't try its file if that's
//!   refused.) Kernels without some of it enforce what they have.
//! - seccomp makes the system calls a plugin has no use for fail: other
//!   processes' memory, kernel modules, mounts, namespaces, BPF,
//!   `io_uring` (which bypasses seccomp), and Internet sockets.
//!
//! What it doesn't stop: anything done over the X server (a plugin's GUI
//! could watch or type into other X11 windows), and reading files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    Scope, path_beneath_rules,
};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use serde::{Deserialize, Serialize};

/// The newest Landlock this knows; older kernels enforce what they can.
const ABI_KNOWN: ABI = ABI::V9;

/// What Dawkey asks of a sandbox, besides the usual: more folders a plugin
/// may write in (where Dawkey keeps the files plugins make).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Sandbox {
    pub writable: Vec<PathBuf>,
}

/// What the kernel enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entered {
    /// Landlock: everything asked, some of it (an older kernel), or none.
    pub(crate) landlock: Enforced,
    pub(crate) seccomp: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Enforced {
    Fully,
    Partly,
    Not,
}

/// The folders a sandboxed plugin may write in: its own config, cache,
/// data and state (the XDG folders), LV2 and VST3 presets, temporary files,
/// devices (`/dev/null`, the GPU), its own `/proc` entries, and `extra`.
pub(crate) fn writable(extra: &[PathBuf]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = |var: &str, fallback: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(fallback)))
    };
    let mut paths: Vec<PathBuf> = [
        xdg("XDG_CONFIG_HOME", ".config"),
        xdg("XDG_CACHE_HOME", ".cache"),
        xdg("XDG_DATA_HOME", ".local/share"),
        xdg("XDG_STATE_HOME", ".local/state"),
        home.as_ref().map(|h| h.join(".lv2")),
        // Where VST3 plugins keep their users' presets.
        home.as_ref().map(|h| h.join(".vst3/presets")),
    ]
    .into_iter()
    .flatten()
    .collect();
    paths.extend(
        ["/tmp", "/var/tmp", "/dev", "/proc/self"]
            .into_iter()
            .map(PathBuf::from),
    );
    paths.extend(extra.iter().cloned());
    paths
}

/// The Unix sockets a sandboxed plugin may connect to: the X server's
/// (under `/tmp`, writable anyway), the Wayland compositor's (floating
/// CLAP GUIs) and the system bus (rtkit). Not the session bus, PipeWire or
/// Dawkey's own MCP socket.
fn sockets() -> Vec<PathBuf> {
    let mut sockets = Vec::new();
    if let Some(display) = std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from) {
        if display.is_absolute() {
            sockets.push(display);
        } else if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
            sockets.push(Path::new(&runtime).join(display));
        }
    }
    let system_bus = std::env::var("DBUS_SYSTEM_BUS_ADDRESS")
        .ok()
        .and_then(|address| {
            address.split(';').find_map(|a| {
                a.strip_prefix("unix:path=")
                    .map(|p| p.split(',').next().unwrap_or(p).to_owned())
            })
        })
        .unwrap_or_else(|| "/run/dbus/system_bus_socket".to_owned());
    sockets.push(PathBuf::from(system_bus));
    sockets
}

/// Puts this process in the sandbox, for good. It must have one thread
/// (Landlock binds the calling thread and those it starts): call it first.
pub(crate) fn enter(sandbox: &Sandbox) -> Result<Entered, String> {
    let threads = std::fs::read_dir("/proc/self/task").map_or(1, |tasks| tasks.count());
    if threads != 1 {
        return Err(format!(
            "the plugin process has {threads} threads; it must have one to enter its sandbox"
        ));
    }
    let landlock = landlock(&writable(&sandbox.writable)).map_err(|e| format!("Landlock: {e}"))?;
    let seccomp = match seccomp() {
        Ok(()) => true,
        Err(e) if landlock == Enforced::Not => {
            return Err(format!("seccomp: {e} (and no Landlock)"));
        }
        Err(e) => {
            tracing::warn!("the plugin sandbox has no seccomp filter: {e}");
            false
        }
    };
    Ok(Entered { landlock, seccomp })
}

fn landlock(writable: &[PathBuf]) -> Result<Enforced, landlock::RulesetError> {
    let abi = ABI_KNOWN;
    let status = Ruleset::default()
        .handle_access(AccessFs::from_write(abi))?
        .handle_access(AccessNet::from_all(abi))?
        .scope(Scope::Signal)?
        .create()?
        // Reading isn't handled, so it's allowed everywhere.
        .add_rules(path_beneath_rules(writable, AccessFs::from_write(abi)))?
        .add_rules(path_beneath_rules(sockets(), AccessFs::ResolveUnix))?
        // No TCP port is allowed.
        .restrict_self()?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => Enforced::Fully,
        RulesetStatus::PartiallyEnforced => Enforced::Partly,
        RulesetStatus::NotEnforced => Enforced::Not,
    })
}

/// System calls that fail with EPERM in the sandbox.
const DENIED: &[libc::c_long] = &[
    // Other processes: debugging them, their memory, their descriptors.
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_pidfd_getfd,
    libc::SYS_kcmp,
    // The kernel and the system.
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_syslog,
    libc::SYS_settimeofday,
    libc::SYS_clock_settime,
    libc::SYS_clock_adjtime,
    libc::SYS_adjtimex,
    libc::SYS_sethostname,
    libc::SYS_setdomainname,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_userfaultfd,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    // Mounts and namespaces (new ones are where kernel bugs hide).
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_move_mount,
    libc::SYS_open_tree,
    libc::SYS_fsopen,
    libc::SYS_fsmount,
    libc::SYS_fsconfig,
    libc::SYS_fspick,
    libc::SYS_mount_setattr,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    // Its system calls aren't filtered.
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

/// Socket families that fail with EACCES: the Internet and raw packets.
const NO_SOCKETS: &[libc::c_int] = &[libc::AF_INET, libc::AF_INET6, libc::AF_PACKET];

/// `clone` flags that make namespaces.
const NEW_NAMESPACES: u64 = (libc::CLONE_NEWNS
    | libc::CLONE_NEWUSER
    | libc::CLONE_NEWPID
    | libc::CLONE_NEWNET
    | libc::CLONE_NEWIPC
    | libc::CLONE_NEWUTS
    | libc::CLONE_NEWCGROUP
    | libc::CLONE_NEWTIME) as u64;

fn seccomp() -> Result<(), String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|e| e.to_string())?;
    let rule = |conditions: Vec<SeccompCondition>| SeccompRule::new(conditions);
    let condition =
        |arg, len, op, value| SeccompCondition::new(arg, len, op, value).map_err(|e| e.to_string());
    let program = |rules: BTreeMap<i64, Vec<SeccompRule>>, errno: libc::c_int| {
        let filter = SeccompFilter::new(
            rules,
            SeccompAction::Allow,
            SeccompAction::Errno(errno as u32),
            arch,
        )
        .map_err(|e| e.to_string())?;
        BpfProgram::try_from(filter).map_err(|e| e.to_string())
    };

    let mut denied: BTreeMap<i64, Vec<SeccompRule>> =
        DENIED.iter().map(|&call| (call, Vec::new())).collect();
    // `clone` with any of the namespace flags (a rule per flag: a call
    // fails if any rule matches).
    let clone = (0..64)
        .map(|bit| 1u64 << bit)
        .filter(|flag| NEW_NAMESPACES & flag != 0)
        .map(|flag| {
            let flag_set = condition(
                0,
                SeccompCmpArgLen::Qword,
                SeccompCmpOp::MaskedEq(flag),
                flag,
            )?;
            rule(vec![flag_set]).map_err(|e| e.to_string())
        })
        .collect::<Result<_, String>>()?;
    denied.insert(libc::SYS_clone, clone);
    let eperm = program(denied, libc::EPERM)?;

    let families = NO_SOCKETS
        .iter()
        .map(|&family| {
            let is = condition(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, family as u64)?;
            rule(vec![is]).map_err(|e| e.to_string())
        })
        .collect::<Result<_, String>>()?;
    let sockets: BTreeMap<i64, Vec<SeccompRule>> = [(libc::SYS_socket, families)].into();
    let eacces = program(sockets, libc::EACCES)?;

    // `clone3` takes its flags in memory, out of seccomp's sight: it says
    // there's no such call, and the C library falls back to `clone`.
    let clone3: BTreeMap<i64, Vec<SeccompRule>> = [(libc::SYS_clone3, Vec::new())].into();
    let enosys = program(clone3, libc::ENOSYS)?;

    for filter in [eperm, eacces, enosys] {
        seccompiler::apply_filter(&filter).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::*;

    /// Runs `inside` in a forked child (one thread, as a plugin process
    /// starts) in the sandbox; returns what it says went wrong, if anything.
    fn in_sandbox(sandbox: &Sandbox, inside: impl FnOnce(&Entered) -> Result<(), String>) {
        let (mut ours, theirs) = UnixStream::pair().unwrap();
        // SAFETY: the child only does what's below, then exits.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            drop(ours);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let entered = enter(sandbox)?;
                inside(&entered)
            }));
            let message = match result {
                Ok(Ok(())) => String::new(),
                Ok(Err(e)) => e,
                Err(_) => "panicked".to_owned(),
            };
            let _ = std::io::Write::write_all(&mut &theirs, message.as_bytes());
            // SAFETY: ends the child without running the parent's exit code.
            unsafe { libc::_exit(0) };
        }
        drop(theirs);
        // A child that hangs (a lock another test's thread held when it
        // forked) fails the test instead of hanging it.
        ours.set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        let mut message = String::new();
        let read = std::io::Read::read_to_string(&mut ours, &mut message);
        let mut status = 0;
        if read.is_err() {
            // SAFETY: our child.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        // SAFETY: our child.
        unsafe { libc::waitpid(pid, &raw mut status, 0) };
        read.expect("the sandboxed child answers");
        assert!(message.is_empty(), "{message}");
    }

    fn errno(result: libc::c_int) -> Option<i32> {
        (result < 0).then(|| std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
    }

    #[test]
    fn plugins_write_only_where_they_may_and_reach_no_network() {
        // Folders outside the usual writable ones (beside the sources, not
        // in /tmp or the user's cache).
        let here = std::env::current_dir().unwrap();
        let elsewhere = tempfile::tempdir_in(&here).unwrap();
        let allowed = tempfile::tempdir_in(&here).unwrap();
        let socket = elsewhere.path().join("app.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let sandbox = Sandbox {
            writable: vec![allowed.path().to_owned()],
        };
        let (elsewhere_path, allowed_path) =
            (elsewhere.path().to_owned(), allowed.path().to_owned());
        // SAFETY: landlock_create_ruleset(NULL, 0, VERSION) asks the ABI.
        let kernel_abi = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<u8>(),
                0,
                1,
            )
        };
        in_sandbox(&sandbox, move |entered| {
            let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_owned()) };
            check(entered.seccomp, "no seccomp")?;
            // Namespaces and the Internet: refused.
            // SAFETY: plain system calls.
            check(
                errno(unsafe { libc::unshare(libc::CLONE_NEWUSER) }) == Some(libc::EPERM),
                "unshare",
            )?;
            for (family, kind) in [
                (libc::AF_INET, libc::SOCK_STREAM),
                (libc::AF_INET6, libc::SOCK_DGRAM),
            ] {
                // SAFETY: as above.
                let made = unsafe { libc::socket(family, kind, 0) };
                check(errno(made) == Some(libc::EACCES), "an Internet socket")?;
            }
            // Threads still start (clone without namespaces).
            check(std::thread::spawn(|| 7).join().ok() == Some(7), "a thread")?;
            if entered.landlock != Enforced::Fully {
                // Only an older kernel (or none, in a container) excuses it.
                return check(kernel_abi < 9, "Landlock isn't fully enforced");
            }
            // Writing: in its folders, not elsewhere; reading anywhere.
            std::fs::write(allowed_path.join("preset"), b"1")
                .map_err(|e| format!("allowed: {e}"))?;
            let denied = std::fs::write(elsewhere_path.join("file"), b"1");
            check(
                denied.is_err_and(|e| e.kind() == ErrorKind::PermissionDenied),
                "writing elsewhere",
            )?;
            check(std::fs::read("Cargo.toml").is_ok(), "reading")?;
            // Other programs' sockets and processes: out of reach.
            check(
                UnixStream::connect(elsewhere_path.join("app.sock")).is_err(),
                "a socket",
            )?;
            // SAFETY: signal 0 only checks.
            check(
                errno(unsafe { libc::kill(libc::getppid(), 0) }) == Some(libc::EPERM),
                "a signal",
            )?;
            Ok(())
        });
        // The child checked writing only where Landlock was fully enforced.
        if kernel_abi >= 9 {
            assert!(!elsewhere.path().join("file").exists());
            assert_eq!(std::fs::read(allowed.path().join("preset")).unwrap(), b"1");
        }
    }

    #[test]
    fn the_usual_folders_are_writable() {
        let paths = writable(&[PathBuf::from("/srv/dawkey")]);
        for path in ["/tmp", "/dev", "/srv/dawkey"] {
            assert!(paths.contains(&PathBuf::from(path)), "{path}");
        }
        if let Some(home) = std::env::var_os("HOME") {
            assert!(paths.contains(&Path::new(&home).join(".lv2")));
            assert!(paths.contains(&Path::new(&home).join(".vst3/presets")));
        }
    }
}
