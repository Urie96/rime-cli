//! Locate librime and emit link flags for rime-sys.
//!
//! Resolution order:
//!   1. `RIME_LIB_DIR` / `RIME_INCLUDE_DIR` environment variables
//!      (used by default.nix and shell.nix)
//!   2. `pkg-config --libs rime` if pkg-config is available
//!   3. Homebrew (`/opt/homebrew/opt/librime`, `/usr/local/opt/librime`)
//!   4. `/usr/local`
//!
//! The include directory is only used to fail fast with a helpful message;
//! the bindings themselves are hand-written and do not need bindgen.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let lib_dir = env::var("RIME_LIB_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(pkg_config_lib_dir)
        .or_else(homebrew_lib_dir)
        .or_else(|| Some(PathBuf::from("/usr/local/lib")));

    let lib_dir = match lib_dir {
        Some(d) if d.join("librime.dylib").exists() || d.join("librime.so").exists() => d,
        other => {
            let hint = other
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "none found".to_string());
            panic!(
                "cannot find librime shared library ({hint}); \
                 install librime or set RIME_LIB_DIR=/path/to/librime/lib"
            );
        }
    };

    let include_dir = env::var("RIME_INCLUDE_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(homebrew_include_dir)
        .unwrap_or_else(|| PathBuf::from("/usr/local/include"));

    if !include_dir.join("rime_api.h").exists() {
        println!(
            "cargo:warning=rime_api.h not found in {} (bindings are hand-written, continuing)",
            include_dir.display()
        );
    }

    println!("cargo:rerun-if-env-changed=RIME_LIB_DIR");
    println!("cargo:rerun-if-env-changed=RIME_INCLUDE_DIR");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=rime");
}

fn pkg_config_lib_dir() -> Option<PathBuf> {
    let out = Command::new("pkg-config").arg("--libs-only-L").arg("rime").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().find_map(|flag| flag.strip_prefix("-L").map(PathBuf::from))
}

fn homebrew_lib_dir() -> Option<PathBuf> {
    ["/opt/homebrew/opt/librime/lib", "/usr/local/opt/librime/lib"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.join("librime.dylib").exists() || p.join("librime.so").exists())
}

fn homebrew_include_dir() -> Option<PathBuf> {
    ["/opt/homebrew/opt/librime/include", "/usr/local/opt/librime/include"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.join("rime_api.h").exists())
}

// keep `Path` import used on all platforms
#[allow(dead_code)]
fn _assert_path(_: &Path) {}
