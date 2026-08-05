use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use gpui_mcp::LiveDocumentSource;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::{
    BindingDocument, BindingDocumentError, HtmlUi, HtmlUiError, LiveHtml, ReloadError, ReloadReport,
};

const HTML_FILE: &str = "app.html";
const CSS_FILE: &str = "app.css";
const BINDINGS_FILE: &str = "app.bindings.ron";
const EVENT_QUEUE_CAPACITY: usize = 128;
const MAX_HTML_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CSS_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BINDINGS_BYTES: u64 = 1024 * 1024;

/// One file in the standard pure-HTML project document.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProjectFile {
    /// `ui/app.html` structure and semantics.
    Html,
    /// `ui/app.css` presentation.
    Css,
    /// `ui/app.bindings.ron` behavior/state connections.
    Bindings,
}

/// Canonical, root-contained paths for a standard pure-HTML project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectPaths {
    root: PathBuf,
    ui_dir: PathBuf,
    html: PathBuf,
    css: PathBuf,
    bindings: PathBuf,
}

impl ProjectPaths {
    /// Open the standard `ui/` document beneath a project root.
    ///
    /// Every file is canonicalized and rejected if it resolves outside the
    /// canonical project root, including through a symlink or junction.
    ///
    /// # Errors
    ///
    /// Returns an error for missing files, non-files, or paths escaping the root.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectError> {
        let requested_root = root.as_ref();
        let root = canonicalize("canonicalize project root", requested_root)?;
        ensure_directory(&root)?;
        let ui_dir = canonicalize_contained("canonicalize UI directory", &root.join("ui"), &root)?;
        ensure_directory(&ui_dir)?;
        let html = open_project_file(&ui_dir.join(HTML_FILE), &root)?;
        let css = open_project_file(&ui_dir.join(CSS_FILE), &root)?;
        let bindings = open_project_file(&ui_dir.join(BINDINGS_FILE), &root)?;
        Ok(Self {
            root,
            ui_dir,
            html,
            css,
            bindings,
        })
    }

    /// Canonical project root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical UI document directory.
    #[must_use]
    pub fn ui_dir(&self) -> &Path {
        &self.ui_dir
    }

    /// Canonical path for one document file.
    #[must_use]
    pub fn file(&self, file: ProjectFile) -> &Path {
        match file {
            ProjectFile::Html => &self.html,
            ProjectFile::Css => &self.css,
            ProjectFile::Bindings => &self.bindings,
        }
    }

    fn all_files() -> BTreeSet<ProjectFile> {
        [ProjectFile::Html, ProjectFile::Css, ProjectFile::Bindings]
            .into_iter()
            .collect()
    }

    fn classify(&self, path: &Path) -> Option<ProjectFile> {
        [ProjectFile::Html, ProjectFile::Css, ProjectFile::Bindings]
            .into_iter()
            .find(|file| paths_equal(path, self.file(*file)))
    }
}

/// Immutable, bounded source bundle loaded from one project revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSnapshot {
    html: String,
    css: String,
    bindings_ron: String,
}

impl ProjectSnapshot {
    /// Read all standard document files with size and root-containment checks.
    ///
    /// # Errors
    ///
    /// Returns an error when a file is replaced by an escaping link, is not a
    /// regular UTF-8 file, exceeds its limit, or cannot be read.
    pub fn load(paths: &ProjectPaths) -> Result<Self, ProjectError> {
        Ok(Self {
            html: read_project_file(paths, ProjectFile::Html, MAX_HTML_BYTES)?,
            css: read_project_file(paths, ProjectFile::Css, MAX_CSS_BYTES)?,
            bindings_ron: read_project_file(paths, ProjectFile::Bindings, MAX_BINDINGS_BYTES)?,
        })
    }

    /// Pure HTML source.
    #[must_use]
    pub fn html(&self) -> &str {
        &self.html
    }

    /// Standard CSS source.
    #[must_use]
    pub fn css(&self) -> &str {
        &self.css
    }

    /// RON binding source.
    #[must_use]
    pub fn bindings_ron(&self) -> &str {
        &self.bindings_ron
    }

    /// Compile this complete source bundle through the fail-closed `HTMLSwap` policy.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bindings, HTML, CSS, or document resolution.
    pub fn compile(&self) -> Result<HtmlUi, ProjectError> {
        let bindings = BindingDocument::from_ron(&self.bindings_ron)?;
        HtmlUi::compile_with_stylesheet(self.html.clone(), bindings, CSS_FILE, self.css.clone())
            .map_err(ProjectError::Compile)
    }

    /// Convert this disk snapshot to the same complete bundle used by MCP preview.
    #[must_use]
    pub fn into_document(self) -> LiveDocumentSource {
        LiveDocumentSource {
            html: self.html,
            css: self.css,
            bindings_ron: self.bindings_ron,
        }
    }
}

/// Coalesced source files implicated by filesystem notifications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectChange {
    files: BTreeSet<ProjectFile>,
    rescan: bool,
}

impl ProjectChange {
    /// Changed standard project files.
    #[must_use]
    pub fn files(&self) -> &BTreeSet<ProjectFile> {
        &self.files
    }

    /// Whether queue overflow requires treating the event as a full rescan.
    #[must_use]
    pub const fn is_rescan(&self) -> bool {
        self.rescan
    }
}

/// One filesystem-driven live-document replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectReload {
    /// Files that caused the reload.
    pub change: ProjectChange,
    /// Atomic live-renderer replacement result.
    pub report: ReloadReport,
}

/// Cross-platform, bounded watcher for the standard project document.
pub struct ProjectWatcher {
    paths: ProjectPaths,
    receiver: Receiver<notify::Result<Event>>,
    overflowed: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
}

impl ProjectWatcher {
    /// Watch the canonical UI directory using the platform-recommended backend.
    ///
    /// The directory, rather than individual files, is watched so atomic-save
    /// rename patterns work on Windows, Linux, and macOS. Events are filtered to
    /// the three exact canonical project paths and delivered through a bounded queue.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating-system watcher cannot be installed.
    pub fn new(paths: ProjectPaths) -> Result<Self, ProjectError> {
        let (sender, receiver) = sync_channel(EVENT_QUEUE_CAPACITY);
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = overflowed.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            if sender.try_send(event).is_err() {
                callback_overflowed.store(true, Ordering::Release);
            }
        })
        .map_err(ProjectError::Watch)?;
        watcher
            .watch(paths.ui_dir(), RecursiveMode::NonRecursive)
            .map_err(ProjectError::Watch)?;
        Ok(Self {
            paths,
            receiver,
            overflowed,
            _watcher: watcher,
        })
    }

    /// Canonical project paths being watched.
    #[must_use]
    pub fn paths(&self) -> &ProjectPaths {
        &self.paths
    }

    /// Drain and coalesce all currently queued events without blocking.
    ///
    /// Polling at a short UI cadence provides natural debounce while preserving
    /// every implicated standard file. A bounded-queue overflow becomes a safe
    /// full rescan rather than silently dropping a change.
    ///
    /// # Errors
    ///
    /// Returns a backend error delivered by the filesystem watcher.
    pub fn poll(&self) -> Result<Option<ProjectChange>, ProjectError> {
        let rescan = self.overflowed.swap(false, Ordering::AcqRel);
        let mut files = if rescan {
            ProjectPaths::all_files()
        } else {
            BTreeSet::new()
        };
        loop {
            match self.receiver.try_recv() {
                Ok(Ok(event)) => {
                    files.extend(
                        event
                            .paths
                            .iter()
                            .filter_map(|path| self.paths.classify(path)),
                    );
                }
                Ok(Err(error)) => return Err(ProjectError::Watch(error)),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if files.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ProjectChange { files, rescan }))
        }
    }

    /// Compile and atomically apply a changed source bundle, if one is queued.
    ///
    /// # Errors
    ///
    /// Read, compile, binding, watcher, and live hook errors are returned while
    /// `LiveHtml` keeps rendering its last-good document.
    pub fn reload_if_changed(
        &self,
        live: &mut LiveHtml,
    ) -> Result<Option<ProjectReload>, ProjectReloadError> {
        let Some(change) = self.poll()? else {
            return Ok(None);
        };
        let candidate = ProjectSnapshot::load(&self.paths)?.compile()?;
        let report = live.reload(candidate)?;
        Ok(Some(ProjectReload { change, report }))
    }
}

/// Failure while opening, reading, watching, or compiling a pure-HTML project.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// Filesystem operation failed.
    #[error("{operation} `{}`", path.display())]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Operating-system error.
        #[source]
        source: std::io::Error,
    },
    /// A project path escaped the canonical root.
    #[error("project path `{}` resolves outside root `{}`", path.display(), root.display())]
    OutsideRoot {
        /// Rejected canonical path.
        path: PathBuf,
        /// Allowed canonical root.
        root: PathBuf,
    },
    /// A required directory was not a directory.
    #[error("required project directory `{}` is not a directory", path.display())]
    NotDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// A required document file was not a regular file.
    #[error("required project file `{}` is not a regular file", path.display())]
    NotFile {
        /// Rejected path.
        path: PathBuf,
    },
    /// A source file exceeded its bounded input limit.
    #[error("project file `{}` is {found} bytes; maximum is {maximum}", path.display())]
    TooLarge {
        /// Rejected path.
        path: PathBuf,
        /// Observed byte length.
        found: u64,
        /// Maximum accepted byte length.
        maximum: u64,
    },
    /// The RON binding document was invalid.
    #[error(transparent)]
    Bindings(#[from] BindingDocumentError),
    /// The HTML/CSS/binding bundle did not compile.
    #[error(transparent)]
    Compile(#[from] HtmlUiError),
    /// Platform filesystem notification failed.
    #[error("watch pure-HTML project")]
    Watch(#[source] notify::Error),
}

/// Failure to apply a filesystem-driven reload.
#[derive(Debug, thiserror::Error)]
pub enum ProjectReloadError {
    /// Reading, watching, or compiling the project failed.
    #[error(transparent)]
    Project(#[from] ProjectError),
    /// The candidate could not connect to the running application's hooks.
    #[error(transparent)]
    Reload(#[from] ReloadError),
}

fn canonicalize(operation: &'static str, path: &Path) -> Result<PathBuf, ProjectError> {
    path.canonicalize().map_err(|source| ProjectError::Io {
        operation,
        path: path.to_owned(),
        source,
    })
}

fn canonicalize_contained(
    operation: &'static str,
    path: &Path,
    root: &Path,
) -> Result<PathBuf, ProjectError> {
    let canonical = canonicalize(operation, path)?;
    if !canonical.starts_with(root) {
        return Err(ProjectError::OutsideRoot {
            path: canonical,
            root: root.to_owned(),
        });
    }
    Ok(canonical)
}

fn ensure_directory(path: &Path) -> Result<(), ProjectError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(ProjectError::NotDirectory {
            path: path.to_owned(),
        })
    }
}

fn open_project_file(path: &Path, root: &Path) -> Result<PathBuf, ProjectError> {
    let canonical = canonicalize_contained("canonicalize project file", path, root)?;
    if canonical.is_file() {
        Ok(canonical)
    } else {
        Err(ProjectError::NotFile { path: canonical })
    }
}

fn read_project_file(
    paths: &ProjectPaths,
    file: ProjectFile,
    maximum: u64,
) -> Result<String, ProjectError> {
    let configured = paths.file(file);
    let canonical = canonicalize_contained("canonicalize project file", configured, paths.root())?;
    if !paths_equal(&canonical, configured) {
        return Err(ProjectError::OutsideRoot {
            path: canonical,
            root: paths.root().to_owned(),
        });
    }
    let metadata = fs::metadata(&canonical).map_err(|source| ProjectError::Io {
        operation: "read project file metadata",
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(ProjectError::NotFile { path: canonical });
    }
    if metadata.len() > maximum {
        return Err(ProjectError::TooLarge {
            path: canonical,
            found: metadata.len(),
            maximum,
        });
    }
    let source = fs::read_to_string(&canonical).map_err(|source| ProjectError::Io {
        operation: "read UTF-8 project file",
        path: canonical.clone(),
        source,
    })?;
    let found = u64::try_from(source.len()).unwrap_or(u64::MAX);
    if found > maximum {
        return Err(ProjectError::TooLarge {
            path: canonical,
            found,
            maximum,
        });
    }
    Ok(source)
}

#[cfg(windows)]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

#[cfg(not(windows))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, Instant};

    use gpui_mcp::Automation;
    use tempfile::TempDir;

    use crate::{BindingDocument, HookRegistry, LiveHtml};

    use super::{ProjectFile, ProjectPaths, ProjectSnapshot, ProjectWatcher};

    fn fixture() -> Result<(TempDir, ProjectPaths), Box<dyn std::error::Error>> {
        let root = TempDir::new()?;
        let ui = root.path().join("ui");
        fs::create_dir(&ui)?;
        fs::write(ui.join("app.html"), "<button id='save'>Save</button>")?;
        fs::write(ui.join("app.css"), "#save { color: red; }")?;
        fs::write(
            ui.join("app.bindings.ron"),
            BindingDocument::new().to_ron_pretty()?,
        )?;
        let paths = ProjectPaths::open(root.path())?;
        Ok((root, paths))
    }

    #[test]
    fn snapshot_loads_and_compiles_the_standard_bundle() -> Result<(), Box<dyn std::error::Error>> {
        let (_root, paths) = fixture()?;
        let snapshot = ProjectSnapshot::load(&paths)?;
        let compiled = snapshot.compile()?;

        assert!(snapshot.html().contains("Save"));
        assert!(compiled.diagnostics().is_empty());
        Ok(())
    }

    #[test]
    fn platform_watcher_coalesces_exact_project_file_changes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_root, paths) = fixture()?;
        let watcher = ProjectWatcher::new(paths.clone())?;
        fs::write(paths.file(ProjectFile::Css), "#save { color: blue; }")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut change = None;
        while change.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            change = watcher.poll()?;
        }

        assert!(change.is_some_and(|change| change.files().contains(&ProjectFile::Css)));
        Ok(())
    }

    #[test]
    fn changed_bundle_reloads_without_recompiling_rust() -> Result<(), Box<dyn std::error::Error>> {
        let (_root, paths) = fixture()?;
        let initial = ProjectSnapshot::load(&paths)?.compile()?;
        let mut live = LiveHtml::new(initial, Automation::for_test(), HookRegistry::new())?;
        let watcher = ProjectWatcher::new(paths.clone())?;
        fs::write(
            paths.file(ProjectFile::Html),
            "<button id='save'>Updated</button>",
        )?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut reload = None;
        while reload.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            reload = watcher.reload_if_changed(&mut live)?;
        }

        assert_eq!(live.revision(), 2);
        assert!(live.document().source().contains("Updated"));
        assert!(reload.is_some());
        Ok(())
    }
}
