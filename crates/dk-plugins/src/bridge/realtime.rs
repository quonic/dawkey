// SPDX-License-Identifier: AGPL-3.0-or-later
//! Real-time scheduling for a plugin process's audio thread.
//!
//! It runs one priority below Dawkey's own audio thread (PipeWire's data
//! loop), so a plugin that takes too long makes its own block late (which
//! Dawkey plays as silence) rather than holding up the whole mix; and only
//! when Dawkey's runs real-time (not on the virtual clock).
//!
//! The thread asks the kernel first, which works where rlimits allow it
//! (`RLIMIT_RTPRIO`, e.g. an `audio` group). Otherwise it asks RealtimeKit
//! on the system bus, as PipeWire does, after setting `RLIMIT_RTTIME` as
//! rtkit requires; and in a Flatpak sandbox, the desktop portal's
//! Realtime interface. Both cap the priority (rtkit at 20 by default), and they're
//! asked from a helper thread, again later while rtkit is busy, so the
//! audio thread never waits for them.

use std::time::Duration;

use zbus::proxy::CacheProperties;

/// How the thread got real-time scheduling, and at what priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Granted {
    Kernel(i32),
    RtKit(i32),
    Portal(i32),
}

/// One below the highest real-time priority among process `pid`'s threads,
/// at least 1; `None` if none of them is real-time.
pub(crate) fn priority_below(pid: u32) -> Option<i32> {
    let tasks = std::fs::read_dir(format!("/proc/{pid}/task")).ok()?;
    tasks
        .filter_map(|task| {
            let stat = std::fs::read_to_string(task.ok()?.path().join("stat")).ok()?;
            realtime_priority(&stat)
        })
        .max()
        .map(|priority| (priority - 1).max(1))
}

/// The real-time priority in a `/proc/.../stat` line, if its policy is
/// `SCHED_FIFO` or `SCHED_RR`.
fn realtime_priority(stat: &str) -> Option<i32> {
    // The name (in parentheses) may hold spaces; fields count after it.
    let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    // Fields 40 (rt_priority) and 41 (policy), counting from 1 with pid
    // and name as the first two.
    let priority: i32 = fields.get(40 - 3)?.parse().ok()?;
    let policy: i32 = fields.get(41 - 3)?.parse().ok()?;
    // The policy may carry SCHED_RESET_ON_FORK.
    let policy = policy & !libc::SCHED_RESET_ON_FORK;
    (matches!(policy, libc::SCHED_FIFO | libc::SCHED_RR) && priority > 0).then_some(priority)
}

/// How many times to ask rtkit (or the portal) while it's busy, waiting
/// twice as long each time (about a minute in all).
const ATTEMPTS: u32 = 6;

/// Makes the calling thread real-time at `priority`: at once if the kernel
/// allows it. Otherwise a helper thread asks rtkit, then the portal, for as
/// close below it as they allow, asking again while they're busy (rtkit
/// grants a few dozen threads per user every 20 seconds, so a project
/// with many plugins can take a while). The calling thread never waits.
pub(crate) fn make_realtime(priority: i32) {
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: this thread, a valid policy and parameter.
    let kernel = unsafe {
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &raw const param)
    };
    if kernel == 0 {
        tracing::debug!(
            "plugin audio thread real-time: {:?}",
            Granted::Kernel(priority)
        );
        return;
    }
    let kernel = std::io::Error::from_raw_os_error(kernel);
    // SAFETY: gettid has no preconditions.
    let tid = unsafe { libc::gettid() };
    let spawned = std::thread::Builder::new()
        .name("dawkey-plugin-rt".into())
        .spawn(move || {
            let mut wait = Duration::from_secs(1);
            for attempt in 1..=ATTEMPTS {
                match ask_for(tid, priority) {
                    Ok(granted) => {
                        tracing::debug!("plugin audio thread real-time: {granted:?}");
                        return;
                    }
                    Err(refused) if refused.busy && attempt < ATTEMPTS => {
                        std::thread::sleep(wait);
                        wait = (wait * 2).min(Duration::from_secs(30));
                    }
                    Err(refused) => {
                        let why = if refused.busy {
                            " (rtkit makes only so many processes real-time per user; \
                             rlimits, e.g. an audio group, have no such limit)"
                        } else {
                            ""
                        };
                        tracing::warn!(
                            "no real-time scheduling for the plugin: the kernel said {kernel}; {}{why}",
                            refused.message
                        );
                        return;
                    }
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("no real-time scheduling for the plugin: {e}");
    }
}

/// Why rtkit and the portal said no, and whether asking again later may
/// work (they were busy: rtkit's burst limit).
#[derive(Debug)]
struct Refused {
    message: String,
    busy: bool,
}

/// Asks rtkit, then in a Flatpak sandbox the portal (which asks rtkit in
/// turn), to make thread `tid` (of this process) real-time.
fn ask_for(tid: libc::pid_t, priority: i32) -> Result<Granted, Refused> {
    let rtkit = match ask(Ask::RtKit, tid, priority) {
        Ok(granted) => return Ok(Granted::RtKit(granted)),
        Err(e) => e,
    };
    if !std::path::Path::new("/.flatpak-info").exists() {
        return Err(Refused {
            busy: is_busy(&rtkit),
            message: format!("rtkit: {rtkit}"),
        });
    }
    let portal = match ask(Ask::Portal, tid, priority) {
        Ok(granted) => return Ok(Granted::Portal(granted)),
        Err(e) => e,
    };
    Err(Refused {
        busy: is_busy(&rtkit) || is_busy(&portal),
        message: format!("rtkit: {rtkit}; the portal: {portal}"),
    })
}

/// rtkit refuses with EBUSY when a user asked too often, or has as many
/// real-time processes as it allows (the portal passes that on).
fn is_busy(message: &str) -> bool {
    message.contains("busy")
}

#[derive(Debug, Clone, Copy)]
enum Ask {
    RtKit,
    Portal,
}

/// Asks rtkit or the portal to make thread `tid` real-time; returns the
/// priority it got.
fn ask(service: Ask, tid: libc::pid_t, priority: i32) -> Result<i32, String> {
    // One call, on a runtime that's gone afterwards (no threads left behind
    // in the plugin process).
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), request(service, tid, priority))
            .await
            .map_err(|_| "no answer".to_owned())?
            .map_err(|e| e.to_string())
    })
}

async fn request(service: Ask, tid: libc::pid_t, priority: i32) -> zbus::Result<i32> {
    let (connection, destination, path, interface) = match service {
        Ask::RtKit => (
            zbus::Connection::system().await?,
            "org.freedesktop.RealtimeKit1",
            "/org/freedesktop/RealtimeKit1",
            "org.freedesktop.RealtimeKit1",
        ),
        Ask::Portal => (
            zbus::Connection::session().await?,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Realtime",
        ),
    };
    let proxy: zbus::Proxy = zbus::proxy::Builder::new(&connection)
        .destination(destination)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    let max: i32 = proxy.get_property("MaxRealtimePriority").await?;
    let rttime: i64 = proxy.get_property("RTTimeUSecMax").await?;
    // Threads only get real-time scheduling from a process that can't use
    // more CPU time than this without blocking (or the kernel stops it).
    limit_rttime(u64::try_from(rttime).unwrap_or(0));
    let priority = priority.min(max).max(1);
    let (pid, tid) = (u64::from(std::process::id()), tid as u64);
    match service {
        Ask::RtKit => {
            proxy
                .call::<_, _, ()>("MakeThreadRealtime", &(tid, priority as u32))
                .await?
        }
        Ask::Portal => {
            proxy
                .call::<_, _, ()>("MakeThreadRealtimeWithPID", &(pid, tid, priority as u32))
                .await?
        }
    }
    Ok(priority)
}

/// Lowers `RLIMIT_RTTIME` to `usec` (never raises it).
fn limit_rttime(usec: u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource and pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &raw mut limit) } != 0 {
        return;
    }
    let usec = usec.min(limit.rlim_max);
    let limit = libc::rlimit {
        rlim_cur: usec,
        rlim_max: usec,
    };
    // SAFETY: a valid resource and limit.
    if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &raw const limit) } != 0 {
        tracing::debug!("setting RLIMIT_RTTIME: {}", std::io::Error::last_os_error());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_priorities_from_stat_lines() {
        // A PipeWire data loop at SCHED_FIFO 55, and the same name with a
        // space and a parenthesis, at SCHED_RR | SCHED_RESET_ON_FORK 20.
        let fifo = "3612354 (data-loop.0) S 1 2 3 0 -1 1077936192 0 0 0 0 0 0 0 0 -56 0 \
                    60 0 100 0 0 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 -1 5 55 1 0 0 0";
        assert_eq!(realtime_priority(fifo), Some(55));
        let rr = format!(
            "5105 (odd) name) S 1 2 3 0 -1 1077936192 0 0 0 0 0 0 0 0 -21 0 \
             60 0 100 0 0 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 -1 5 20 {} 0 0 0",
            libc::SCHED_RR | libc::SCHED_RESET_ON_FORK
        );
        assert_eq!(realtime_priority(&rr), Some(20));
        let other = fifo.replace(" 55 1 0 0 0", " 0 0 0 0 0");
        assert_eq!(realtime_priority(&other), None);
        assert_eq!(realtime_priority("garbage"), None);
    }

    #[test]
    fn a_process_without_real_time_threads_has_no_priority() {
        // The test process runs nothing real-time.
        assert_eq!(priority_below(std::process::id()), None);
        assert_eq!(priority_below(u32::MAX), None);
    }

    #[test]
    fn busy_refusals_are_asked_again() {
        assert!(is_busy(
            "org.freedesktop.DBus.Error.AccessDenied: Device or resource busy"
        ));
        assert!(!is_busy(
            "org.freedesktop.DBus.Error.AccessDenied: Operation not permitted"
        ));
    }

    /// Asks the desktop's rtkit for another thread, as the helper does
    /// (needs a session with rtkit running):
    /// `cargo test -p dk-plugins realtime -- --ignored`.
    #[test]
    #[ignore]
    fn rtkit_makes_a_thread_real_time() {
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let target = std::thread::spawn(move || {
            // SAFETY: gettid has no preconditions.
            tid_tx.send(unsafe { libc::gettid() }).unwrap();
            go_rx.recv().unwrap();
            let mut policy = 0;
            let mut param = libc::sched_param { sched_priority: 0 };
            // SAFETY: this thread, valid pointers.
            let r = unsafe {
                libc::pthread_getschedparam(libc::pthread_self(), &raw mut policy, &raw mut param)
            };
            assert_eq!(r, 0);
            (policy & !libc::SCHED_RESET_ON_FORK, param.sched_priority)
        });
        let granted = ask(Ask::RtKit, tid_rx.recv().unwrap(), 10).unwrap();
        assert!((1..=10).contains(&granted));
        go_tx.send(()).unwrap();
        assert_eq!(target.join().unwrap(), (libc::SCHED_RR, granted));
    }
}
