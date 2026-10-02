//! `cargo xtask vendor`: reproduce a patched crates.io GPUI crate, or verify
//! the vendored copy without changing files.

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::{Args, ValueEnum};
use sha2::{Digest as _, Sha256};

use crate::patch::{self, Files};

/// The patches in each version's series, applied in this order.
const SERIES: [Patch; 3] = [Patch::Automation, Patch::FontFallback, Patch::Grid];

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Crate {
    /// The release GPUI Kit and gpui-component use.
    #[value(name = "gpui-pre")]
    GpuiPre,
    /// The community fork of GPUI.
    #[value(name = "gpui-ce")]
    GpuiCe,
}

impl Crate {
    fn name(self) -> &'static str {
        match self {
            Self::GpuiPre => "gpui-pre",
            Self::GpuiCe => "gpui-ce",
        }
    }

    /// The crate's key in the workspace's `[workspace.dependencies]`.
    fn dependency_key(self) -> &'static str {
        match self {
            Self::GpuiPre => "gpui_pre",
            Self::GpuiCe => "gpui_ce",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Patch {
    /// Everything the bridge needs.
    Automation,
    /// An unrelated font fix that consumers may skip.
    FontFallback,
    /// CSS grid track lists, which gpui-mcp-html renders `grid-template-*` with.
    /// Consumers that don't use gpui-mcp-html may skip it.
    Grid,
}

impl Patch {
    fn name(self) -> &'static str {
        match self {
            Self::Automation => "automation",
            Self::FontFallback => "font-fallback",
            Self::Grid => "grid",
        }
    }
}

#[derive(Args)]
pub(crate) struct VendorArgs {
    /// The crate to patch.
    #[arg(long = "crate", value_enum, default_value = "gpui-pre")]
    krate: Crate,
    /// A supported release (default: the vendored one).
    #[arg(long)]
    version: Option<String>,
    /// Write the patched crate here instead of vendor/<crate>.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Leave out an optional patch: font-fallback or grid.
    #[arg(long, value_enum)]
    without: Vec<Patch>,
    /// Verify vendor/<crate> instead of writing anything.
    #[arg(long, conflicts_with = "output")]
    check: bool,
}

pub(crate) fn run(root: &Path, args: &VendorArgs) -> Result<()> {
    let name = args.krate.name();
    if args.without.contains(&Patch::Automation) {
        bail!("the automation patch is what the bridge needs and can't be left out");
    }
    let vendor = root.join("vendor").join(name);
    let patches = root.join("vendor/patches").join(name);
    let metadata: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(root.join(format!("vendor/{name}.json")))
            .with_context(|| format!("reading vendor/{name}.json"))?,
    )?;
    let vendored = metadata["vendored"]
        .as_str()
        .context("missing `vendored`")?;
    let supported = metadata["versions"]
        .as_object()
        .context("missing `versions`")?;
    check_requirement(root, args.krate, supported.keys().map(String::as_str))?;
    for version in supported.keys() {
        for patch in SERIES {
            let path = patches
                .join(version)
                .join(format!("{}.patch", patch.name()));
            if !path.is_file() {
                bail!(
                    "missing vendor/patches/{name}/{version}/{}.patch",
                    patch.name()
                );
            }
        }
    }

    let version = args.version.as_deref().unwrap_or(vendored);
    let Some(release) = supported.get(version) else {
        let mut versions: Vec<_> = supported.keys().map(String::as_str).collect();
        versions.sort_by_key(|version| version_key(version));
        bail!(
            "{name} {version} has no patch series; supported: {}",
            versions.join(", ")
        );
    };
    if args.output.is_none() && (version != vendored || !args.without.is_empty()) {
        bail!("vendor/{name} holds the full series for the vendored release; use --output");
    }
    let destination = match &args.output {
        Some(output) => std::path::absolute(output)?,
        None => vendor.clone(),
    };
    if destination.exists() && !is_crate(&destination, name) {
        bail!(
            "refusing to replace {}: it is not a {name} crate",
            destination.display()
        );
    }

    let applied: Vec<_> = SERIES
        .into_iter()
        .filter(|patch| !args.without.contains(patch))
        .collect();
    let files = patched_release(name, version, release, &patches.join(version), &applied)?;
    let applied: Vec<_> = applied.iter().map(|patch| patch.name()).collect();

    if args.check {
        let actual = read_tree(&vendor)?;
        let changed: std::collections::BTreeSet<_> = files
            .keys()
            .chain(actual.keys())
            .filter(|path| files.get(*path) != actual.get(*path))
            .map(|path| path.display().to_string())
            .collect();
        if !changed.is_empty() {
            let changed: Vec<_> = changed.into_iter().collect();
            bail!("vendored snapshot differs:\n{}", changed.join("\n"));
        }
        println!("{name} {version}: vendored snapshot matches archive plus patches");
    } else {
        replace(&destination, &files)?;
        println!(
            "{name} {version} ({}): written to {}",
            applied.join(", "),
            destination.display()
        );
    }
    Ok(())
}

/// Download a release, check it, and apply the given patches from `series`.
fn patched_release(
    name: &str,
    version: &str,
    release: &serde_json::Value,
    series: &Path,
    applied: &[Patch],
) -> Result<Files> {
    let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
    let archive = download(&url)?;
    let sha256 = release["sha256"].as_str().context("missing `sha256`")?;
    if format!("{:x}", Sha256::digest(&archive)) != sha256 {
        bail!("{name} archive checksum mismatch");
    }
    let mut files = extract(&archive, &format!("{name}-{version}"))?;
    // gpui-pre records the Zed commit it was cut from; gpui-ce does not.
    if let Some(zed_rev) = release.get("zed_rev").and_then(serde_json::Value::as_str) {
        let manifest: toml::Value = toml::from_str(std::str::from_utf8(
            files
                .get(Path::new("Cargo.toml"))
                .context("archive has no Cargo.toml")?,
        )?)?;
        let recorded = manifest["package"]["metadata"]["gpui-pre"]["zed-rev"].as_str();
        if recorded != Some(zed_rev) {
            bail!("archive Zed revision does not match vendor/{name}.json");
        }
    }
    // The publisher ships CRLF sources; keep source patches independent of it.
    for (path, content) in &mut files {
        if path.starts_with("src") && path.extension().is_some_and(|ext| ext == "rs") {
            *content = crlf_to_lf(content);
        }
    }
    files.remove(Path::new("Cargo.lock"));
    for patch in applied {
        let file = format!("{}.patch", patch.name());
        let text = fs::read_to_string(series.join(&file))?;
        patch::apply(&mut files, &text)
            .with_context(|| format!("applying vendor/patches/{name}/{version}/{file}"))?;
    }
    Ok(files)
}

fn crlf_to_lf(content: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(content.len());
    let mut bytes = content.iter().peekable();
    while let Some(&byte) = bytes.next() {
        if !(byte == b'\r' && bytes.peek() == Some(&&b'\n')) {
            normalized.push(byte);
        }
    }
    normalized
}

/// The bridge must accept exactly the releases that have a patch series.
fn check_requirement<'a>(
    root: &Path,
    krate: Crate,
    versions: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let key = krate.dependency_key();
    let workspace: toml::Value = toml::from_str(&fs::read_to_string(root.join("Cargo.toml"))?)?;
    let requirement = workspace["workspace"]["dependencies"][key]["version"]
        .as_str()
        .with_context(|| format!("workspace dependency {key} has no version"))?;
    let mut versions: Vec<_> = versions.collect();
    versions.sort_by_key(|version| version_key(version));
    let expected = match versions.as_slice() {
        [] => bail!("vendor/{}.json lists no versions", krate.name()),
        [only] => format!("={only}"),
        [first, .., last] => format!(">={first}, <={last}"),
    };
    if requirement != expected {
        bail!(
            "workspace {key} requirement {requirement:?} does not match the versions in \
             vendor/{}.json; expected {expected:?}",
            krate.name()
        );
    }
    Ok(())
}

fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn is_crate(directory: &Path, name: &str) -> bool {
    fs::read_to_string(directory.join("Cargo.toml"))
        .ok()
        .and_then(|manifest| toml::from_str::<toml::Value>(&manifest).ok())
        .and_then(|manifest| {
            manifest
                .get("package")?
                .get("name")?
                .as_str()
                .map(|found| found == name)
        })
        .unwrap_or(false)
}

fn download(url: &str) -> Result<Vec<u8>> {
    use ureq::tls::{RootCerts, TlsConfig};
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_global(Some(std::time::Duration::from_mins(2)))
        .build()
        .into();
    let mut body = Vec::new();
    agent
        .get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)?;
    Ok(body)
}

/// Read a published crate archive. Published GPUI archives contain regular
/// files and directories only, all under `prefix`.
fn extract(archive: &[u8], prefix: &str) -> Result<Files> {
    let mut files = Files::new();
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        let mut components = path.components();
        let valid = components.next() == Some(Component::Normal(prefix.as_ref()))
            && components
                .clone()
                .all(|component| matches!(component, Component::Normal(_)));
        if !valid || !(kind.is_file() || kind.is_dir()) {
            bail!("unexpected archive member: {}", path.display());
        }
        if kind.is_file() {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            files.insert(components.as_path().to_path_buf(), content);
        }
    }
    Ok(files)
}

fn read_tree(directory: &Path) -> Result<Files> {
    let mut files = Files::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in
            fs::read_dir(&current).with_context(|| format!("reading {}", current.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(
                    path.strip_prefix(directory)?.to_path_buf(),
                    fs::read(&path)?,
                );
            }
        }
    }
    Ok(files)
}

/// Write `files` beside `destination`, then swap it into place, so a failure
/// leaves any existing copy untouched.
fn replace(destination: &Path, files: &Files) -> Result<()> {
    let parent = destination
        .parent()
        .context("destination has no parent directory")?;
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".gpui-vendor-")
        .tempdir_in(parent)?;
    let fresh = staging.path().join("fresh");
    for (path, content) in files {
        let path = fresh.join(path);
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::write(&path, content)?;
    }
    let previous = staging.path().join("previous");
    if destination.exists() {
        fs::rename(destination, &previous)?;
    }
    if let Err(error) = fs::rename(&fresh, destination) {
        if previous.exists() {
            fs::rename(&previous, destination)?;
        }
        return Err(error.into());
    }
    Ok(())
}
