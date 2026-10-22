//! `cargo xtask check-consumer`: build a standalone app that installs the
//! bridge from a published Git commit.
//!
//! The app follows the README's recipe for one backend: GPUI Kit on gpui-pre,
//! or gpui-ce with its platform crate. It type-checks `BridgeHandle::install`
//! with that backend's `Window` and `App`, and verifies that the app's GPUI
//! crates and the bridge all share the one patched GPUI package from the
//! requested commit.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};
use clap::{Args, ValueEnum};
use serde_json::Value;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Backend {
    /// GPUI Kit 0.8.0 on gpui-pre 0.3.9.
    #[value(name = "gpui-kit")]
    GpuiKit,
    /// gpui-ce 0.2.2 with `gpui_ce_platform`.
    #[value(name = "gpui-ce")]
    GpuiCe,
}

struct Recipe {
    /// The app's own dependencies.
    dependencies: &'static str,
    /// The bridge feature that selects this backend.
    feature: &'static str,
    /// The crates.io GPUI package the Git patch replaces, and its version.
    gpui: (&'static str, &'static str),
    /// The path the app reaches GPUI's types through.
    types: &'static str,
    /// Every package that must link the one patched GPUI.
    owners: &'static [&'static str],
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::GpuiKit => "gpui-kit",
            Self::GpuiCe => "gpui-ce",
        }
    }

    fn recipe(self) -> Recipe {
        match self {
            Self::GpuiKit => Recipe {
                dependencies: "gpui-kit = \"=0.8.0\"",
                feature: "gpui-pre",
                gpui: ("gpui-pre", "0.3.9"),
                types: "gpui_kit",
                owners: &[
                    "gpui-base",
                    "gpui-component",
                    "gpui-kit",
                    "gpui-mcp",
                    "gpui-pre-platform",
                ],
            },
            Self::GpuiCe => Recipe {
                dependencies: "gpui-ce = \"=0.2.2\"\ngpui_ce_platform = \"=0.1.0\"",
                feature: "gpui-ce",
                gpui: ("gpui-ce", "0.2.2"),
                types: "gpui",
                owners: &["gpui-mcp", "gpui_ce_platform"],
            },
        }
    }
}

#[derive(Args)]
pub(crate) struct ConsumerArgs {
    /// Which README recipe to check.
    #[arg(long, value_enum, default_value = "gpui-kit")]
    backend: Backend,
    /// The gpui-mcp Git repository URL.
    #[arg(long)]
    repository: String,
    /// The full commit SHA to install from.
    #[arg(long)]
    rev: String,
    /// An optional shared Cargo build cache.
    #[arg(long)]
    target_dir: Option<PathBuf>,
}

pub(crate) fn run(args: &ConsumerArgs) -> Result<()> {
    ensure!(
        args.rev.len() == 40 && args.rev.chars().all(|c| c.is_ascii_hexdigit()),
        "--rev must be a full 40-character commit SHA"
    );
    let recipe = args.backend.recipe();
    let (gpui_package, gpui_version) = recipe.gpui;
    let repository = serde_json::to_string(&args.repository)?;
    let rev = serde_json::to_string(&args.rev)?;
    let target_dir = args
        .target_dir
        .as_deref()
        .map(std::path::absolute)
        .transpose()?;

    let consumer = tempfile::Builder::new()
        .prefix(&format!("{}-consumer-", args.backend.name()))
        .tempdir()?;
    let root = consumer.path();
    fs::create_dir(root.join("src"))?;
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{name}-git-consumer"
version = "0.0.0"
edition = "2024"
rust-version = "1.96"

[workspace]

[dependencies]
{dependencies}
gpui-mcp = {{ git = {repository}, rev = {rev}, default-features = false, features = ["{feature}"] }}

[patch.crates-io]
{gpui_package} = {{ git = {repository}, rev = {rev} }}
"#,
            name = args.backend.name(),
            dependencies = recipe.dependencies,
            feature = recipe.feature,
        ),
    )?;
    fs::write(
        root.join("src/main.rs"),
        format!(
            r#"use gpui_mcp::{{AppId, BridgeConfig, BridgeHandle}};

fn install(window: &mut {types}::Window, cx: &mut {types}::App)
    -> Result<BridgeHandle, Box<dyn std::error::Error>>
{{
    Ok(BridgeHandle::install(
        window, cx, BridgeConfig::new(AppId::new("git-consumer")?, "Git consumer"),
    )?)
}}

fn main() {{
    // Type-check real bridge installation with the backend's GPUI types.
    let _ = install;
}}
"#,
            types = recipe.types,
        ),
    )?;

    let cargo = |arguments: &[&str]| {
        let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
        command.args(arguments).current_dir(root);
        if let Some(target_dir) = &target_dir {
            command.env("CARGO_TARGET_DIR", target_dir);
        }
        command
    };
    let status = cargo(&["check"]).status()?;
    ensure!(
        status.success(),
        "cargo check failed for the {} consumer",
        args.backend.name()
    );
    let output = cargo(&["metadata", "--format-version=1", "--locked"]).output()?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    verify(&metadata, &recipe, &args.rev)?;
    println!(
        "{}: all {} GPUI consumers use patched {gpui_package} {gpui_version} at {}",
        args.backend.name(),
        recipe.owners.len(),
        args.rev
    );
    Ok(())
}

/// Check that exactly one patched GPUI package from `rev` was resolved, and
/// that every owner links it and no other GPUI.
fn verify(metadata: &Value, recipe: &Recipe, rev: &str) -> Result<()> {
    let (gpui_package, gpui_version) = recipe.gpui;
    let packages = metadata["packages"]
        .as_array()
        .context("metadata has no packages")?;
    let packages_named = |name: &str| -> Vec<&Value> {
        packages
            .iter()
            .filter(|package| package["name"] == name)
            .collect()
    };
    let [snapshot] = packages_named(gpui_package)[..] else {
        bail!(
            "expected one {gpui_package} package; found {}",
            packages_named(gpui_package).len()
        );
    };
    let source = snapshot["source"].as_str().unwrap_or_default();
    if snapshot["version"] != gpui_version || !source.ends_with(&format!("#{}", rev.to_lowercase()))
    {
        bail!("{gpui_package} did not resolve to the requested patched Git snapshot");
    }
    let names: HashMap<&str, &str> = packages
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package["name"].as_str()?)))
        .collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .context("metadata has no resolve graph")?
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect();
    for owner in recipe.owners {
        let [package] = packages_named(owner)[..] else {
            bail!(
                "expected one {owner} package; found {}",
                packages_named(owner).len()
            );
        };
        let id = package["id"].as_str().context("package has no id")?;
        let gpui: Vec<&str> = nodes[id]["deps"]
            .as_array()
            .context("node has no deps")?
            .iter()
            .filter_map(|dep| dep["pkg"].as_str())
            .filter(|pkg| matches!(names.get(pkg), Some(&("gpui" | "gpui-pre" | "gpui-ce"))))
            .collect();
        if gpui != [snapshot["id"].as_str().unwrap_or_default()] {
            bail!("{owner} does not use exactly the patched {gpui_package} package: {gpui:?}");
        }
    }
    Ok(())
}
