mod build_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use build_support::{
    BRIDGE_INPUTS, BRIDGE_MODE_ENV, BridgeBuildResult, BridgeMode, PostBuildDecision,
    PreBuildDecision, bridge_build_paths, parse_bridge_mode, post_build_decision,
    pre_build_decision, staged_bridge_dir,
};

const DISABLE_SWIFT_SANDBOX_ENV: &str = "GRAPHA_SWIFT_BUILD_DISABLE_SANDBOX";

fn main() {
    println!("cargo::rustc-check-cfg=cfg(no_swift_bridge)");
    println!("cargo:rerun-if-env-changed={BRIDGE_MODE_ENV}");
    println!("cargo:rerun-if-env-changed={DISABLE_SWIFT_SANDBOX_ENV}");

    let mode = parse_bridge_mode(std::env::var(BRIDGE_MODE_ENV).ok().as_deref())
        .unwrap_or_else(|err| panic!("{err}"));

    for path in BRIDGE_INPUTS {
        println!("cargo:rerun-if-changed={path}");
    }

    if std::env::var_os("CARGO_FEATURE_SWIFT_OPTIMIZATIONS").is_none() {
        disable_bridge("swift-optimizations feature disabled (using tree-sitter)");
        return;
    }

    if mode == BridgeMode::Off {
        disable_bridge("off (skipping Swift bridge build)");
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("Cargo should always provide CARGO_MANIFEST_DIR to build scripts");
    let bridge_dir = Path::new(&manifest_dir).join("swift-bridge");
    let package_manifest = bridge_dir.join("Package.swift");
    let out_dir = PathBuf::from(
        std::env::var("OUT_DIR").expect("Cargo should always provide OUT_DIR to build scripts"),
    );
    let (scratch_dir, lib_path) = bridge_build_paths(&out_dir);
    let staged_dir = staged_bridge_dir(&out_dir);
    match pre_build_decision(mode, package_manifest.exists()) {
        PreBuildDecision::Skip(message) => {
            disable_bridge(message);
            return;
        }
        PreBuildDecision::Build => {}
        PreBuildDecision::Panic(message) => panic!("{message}"),
    }

    let mut swift_build = Command::new("swift");
    swift_build.arg("build");

    if disable_swift_sandbox() {
        swift_build.arg("--disable-sandbox");
    }

    swift_build.arg("--scratch-path").arg(&scratch_dir);

    let status = swift_build
        .args(["-c", "release"])
        .current_dir(&bridge_dir)
        .status();

    let build_result = match status {
        Ok(s) if s.success() && lib_path.join("libGraphaSwiftBridge.dylib").exists() => {
            BridgeBuildResult::Success
        }
        Ok(s) if s.success() => BridgeBuildResult::MissingDylib,
        Ok(_) => BridgeBuildResult::FailedStatus,
        Err(_) => BridgeBuildResult::LaunchFailed,
    };

    match post_build_decision(mode, build_result) {
        PostBuildDecision::EnableBridge => {
            println!("cargo:warning=Swift bridge mode: {}", mode.as_str());
            match stage_bridge_artifact(&lib_path, &staged_dir) {
                Ok(staged_dylib) => {
                    println!(
                        "cargo:rustc-env=SWIFT_BRIDGE_PATH={}",
                        staged_dylib
                            .parent()
                            .expect("staged dylib should always have a parent directory")
                            .display()
                    );
                }
                Err(err) if mode == BridgeMode::Auto => {
                    println!(
                        "cargo:warning=Swift bridge mode: auto (failed to stage bridge dylib into `{}`, using scratch output: {err})",
                        staged_dir.display()
                    );
                    println!("cargo:rustc-env=SWIFT_BRIDGE_PATH={}", lib_path.display());
                }
                Err(err) => {
                    panic!(
                        "Swift bridge mode {}: failed to stage `{}` into `{}`: {err}",
                        mode.as_str(),
                        lib_path.join("libGraphaSwiftBridge.dylib").display(),
                        staged_dir.display(),
                    );
                }
            }
        }
        PostBuildDecision::Disable(message) => {
            disable_bridge(message);
        }
        PostBuildDecision::Panic(message) => {
            panic!("{message}");
        }
    }
}

fn disable_bridge(message: &str) {
    println!("cargo:warning=Swift bridge mode: {message}");
    println!("cargo:rustc-cfg=no_swift_bridge");
}

fn disable_swift_sandbox() -> bool {
    matches!(
        std::env::var(DISABLE_SWIFT_SANDBOX_ENV)
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn stage_bridge_artifact(lib_dir: &Path, staged_dir: &Path) -> std::io::Result<PathBuf> {
    let source = lib_dir.join("libGraphaSwiftBridge.dylib");
    let destination = staged_dir.join("libGraphaSwiftBridge.dylib");

    fs::create_dir_all(staged_dir)?;
    fs::copy(&source, &destination)?;

    Ok(destination)
}
