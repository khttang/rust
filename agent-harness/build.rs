//! Embeds the build's identity for the audit trail (configuration
//! identification, see docs/audit-and-certification.md): the compiler
//! version, target triple and profile. Every `run_started` record carries
//! them through `audit::BuildInfo`.
//!
//! Cargo rebuilds everything, this script included, when the compiler
//! changes. `AGENT_HARNESS_GIT_COMMIT` is read by the crate itself with
//! `option_env!`, so a new value also triggers a rebuild.

use std::{env, process::Command};

fn main() {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let version = Command::new(&rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=AGENT_HARNESS_RUSTC_VERSION={version}");
    println!(
        "cargo:rustc-env=AGENT_HARNESS_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=AGENT_HARNESS_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_default()
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=AGENT_HARNESS_GIT_COMMIT");
}
