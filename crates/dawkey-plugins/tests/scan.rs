// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scanning plugin files in processes of their own, with `/bin/sh` standing
//! in for the scanner.

use std::path::Path;
use std::time::{Duration, Instant};

use dawkey_plugins::{SearchPath, scan};

/// A search path with one `.clap` file, `name` (the test library).
fn one_clap_file(dir: &Path, name: &str) -> SearchPath {
    let clap_dir = dir.join("clap");
    std::fs::create_dir(&clap_dir).unwrap();
    std::os::unix::fs::symlink(dawkey_test_plugins::library().unwrap(), clap_dir.join(name))
        .unwrap();
    SearchPath {
        clap: vec![clap_dir],
        lv2: Some(Vec::new()),
        vst3: Vec::new(),
    }
}

/// A plugin that starts a helper process while it's scanned (wine's
/// wineserver for yabridge's plugins, a licence daemon) leaves the scan's
/// stdout pipe open in it: the scanner exits, and reading its output
/// mustn't wait for the helper to go too.
#[test]
fn a_helper_process_holding_the_scanners_stdout_doesnt_stall_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let search = one_clap_file(dir.path(), "helper.clap");
    let args = ["-c", "echo '[]'; sleep 20 &"];
    let started = Instant::now();
    let catalog = scan(
        &search,
        Some((Path::new("/bin/sh"), &args[..])),
        Duration::from_secs(2),
    );
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(2),
        "the scan took {took:?} (timeout 2 s)"
    );
    // What it printed before it exited still counts.
    assert!(catalog.failures.is_empty(), "{:?}", catalog.failures);
}
