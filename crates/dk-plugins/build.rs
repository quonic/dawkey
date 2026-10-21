// SPDX-License-Identifier: AGPL-3.0-or-later
//! Builds the Qt shim (`qt/shim.cpp`) against Qt 5 and Qt 6, where their
//! development files are installed, into shared objects that `qt.rs`
//! embeds (`cfg(dawkey_qt5)`, `cfg(dawkey_qt6)`). Without them Dawkey builds
//! all the same, and says so when a plugin's Qt UI is opened.
//! `DAWKEY_NO_QT=1` skips them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rerun-if-changed=qt/shim.cpp");
    for var in ["DAWKEY_NO_QT", "CXX", "PKG_CONFIG_PATH"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    println!("cargo::rustc-check-cfg=cfg(dawkey_qt5)");
    println!("cargo::rustc-check-cfg=cfg(dawkey_qt6)");
    if std::env::var_os("DAWKEY_NO_QT").is_some_and(|v| !v.is_empty() && v != "0") {
        return;
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    for (module, name) in [("Qt5Widgets", "qt5"), ("Qt6Widgets", "qt6")] {
        match build(module, &out.join(format!("libdawkey-{name}.so"))) {
            Ok(()) => println!("cargo::rustc-cfg=dawkey_{name}"),
            Err(why) => println!("cargo::warning=no {module} support for plugins' Qt UIs: {why}"),
        }
    }
}

/// Compiles the shim against `module` (a pkg-config module) into `out`.
fn build(module: &str, out: &Path) -> Result<(), String> {
    let flags = Command::new("pkg-config")
        .args(["--cflags", "--libs", module])
        .output()
        .map_err(|e| format!("running pkg-config: {e}"))?;
    if !flags.status.success() {
        return Err(format!("pkg-config doesn't know {module}"));
    }
    let flags = String::from_utf8_lossy(&flags.stdout).into_owned();
    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_owned());
    let built = Command::new(&cxx)
        .args([
            "-std=c++17",
            "-O2",
            "-fPIC",
            "-shared",
            "-fvisibility=hidden",
            "-s",
        ])
        .arg("qt/shim.cpp")
        .arg("-o")
        .arg(out)
        .args(flags.split_whitespace())
        .output()
        .map_err(|e| format!("running {cxx}: {e}"))?;
    if !built.status.success() {
        return Err(format!(
            "{cxx} failed: {}",
            String::from_utf8_lossy(&built.stderr).trim()
        ));
    }
    Ok(())
}
