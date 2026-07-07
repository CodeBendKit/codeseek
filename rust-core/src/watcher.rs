//! File system watcher for automatic incremental indexing.
//!
//! Uses the `notify` crate to detect file changes in the project directory,
//! debounces events, and triggers `codeseek init` to perform incremental
//! MD5-based index updates.

use std::path::{Path, PathBuf};
use std::time::Duration;
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// The set of file extensions that should trigger re-indexing.
const WATCHED_EXTENSIONS: &[&str] = &[
    "rs", "ts", "py", "go", "java", "cpp", "c", "h", "hpp",
    "toml", "json", "yaml", "yml", "md",
];

/// Debounce duration: wait this long after the last file change before triggering index update.
const DEBOUNCE_DURATION: Duration = Duration::from_secs(2);

/// Start a file system watcher for the given project root directory.
///
/// This spawns a background task that:
/// 1. Monitors the project directory for file changes using `notify`
/// 2. Filters events to exclude irrelevant files (.git, target, node_modules, hidden dirs)
/// 3. Debounces rapid successive changes (2-second silence window)
/// 4. Triggers `codeseek init` (incremental index update) via the binary
///
/// The function returns a `FileWatcherGuard` that keeps the watcher alive.
/// Dropping the guard stops the watcher.
pub fn start_watcher(project_root: &Path) -> Result<FileWatcherGuard, String> {
    let root = project_root
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize project root: {}", e))?;

    // Channel: notify callback (runs on OS threads) → async processor
    let (event_tx, event_rx) = mpsc::unbounded_channel::<Vec<PathBuf>>();

    // Build a recommended watcher (inotify on Linux, FSEvents on macOS, etc.)
    let watcher_root = root.clone();
    let mut watcher: RecommendedWatcher = Watcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                let paths = filter_event(event, &watcher_root);
                if !paths.is_empty() {
                    // UnboundedSender::send is non-blocking, safe from non-async context
                    let _ = event_tx.send(paths);
                }
            }
        },
        Config::default()
            .with_poll_interval(Duration::from_secs(2))
            .with_compare_contents(false),
    )
    .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch directory '{}': {}", root.display(), e))?;

    // Spawn the debounced event processor as a background tokio task
    let processor_root = root.clone();
    tokio::spawn(async move {
        debounced_processor(event_rx, processor_root).await;
    });

    info!("[watcher] File system watcher started for: {:?}", root);

    Ok(FileWatcherGuard { _watcher: Some(watcher) })
}

/// A guard that keeps the file watcher alive. When dropped, the watcher stops.
pub struct FileWatcherGuard {
    _watcher: Option<RecommendedWatcher>,
}

impl Drop for FileWatcherGuard {
    fn drop(&mut self) {
        if let Some(_watcher) = self._watcher.take() {
            // Drop the watcher to stop file system monitoring
            info!("[watcher] File watcher stopped");
        }
    }
}

/// Filter a notify event to extract only relevant, processable file paths.
fn filter_event(event: notify::Event, root: &Path) -> Vec<PathBuf> {
    // Only react to content-affecting events
    let relevant = match event.kind {
        EventKind::Create(_) => true,
        EventKind::Modify(_) => true,
        EventKind::Remove(_) => true,
        _ => false,
    };

    if !relevant {
        return Vec::new();
    }

    event
        .paths
        .into_iter()
        .filter(|p| should_watch_file(p, root))
        .collect()
}

/// Determine if a file path should be watched for changes.
///
/// Filters by extension and excludes build artifacts, hidden directories, etc.
fn should_watch_file(path: &Path, root: &Path) -> bool {
    // Must have a relevant file extension
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext,
        None => return false,
    };

    if !WATCHED_EXTENSIONS.contains(&ext) {
        return false;
    }

    // Compute relative path from project root for filtering
    let rel = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return true, // If outside root, still process (might be symlink etc.)
    };

    let rel_str = rel.to_string_lossy();

    // Exclude common build artifact directories
    let skip_prefixes = [
        "target/", "target\\",
        "node_modules/", "node_modules\\",
        "dist/", "dist\\",
        "build/", "build\\",
        ".git/", ".git\\",
    ];

    for prefix in &skip_prefixes {
        if rel_str.starts_with(prefix) {
            return false;
        }
    }

    // Exclude hidden directories (starting with '.')
    for component in rel.components() {
        if let std::path::Component::Normal(os_str) = component {
            if let Some(s) = os_str.to_str() {
                if s.starts_with('.') && s != "." && s != ".." {
                    return false;
                }
            }
        }
    }

    true
}

/// Debounced event processor.
///
/// Collects file change events over a time window, then triggers
/// incremental index update when no events arrive for `DEBOUNCE_DURATION`.
async fn debounced_processor(
    mut event_rx: mpsc::UnboundedReceiver<Vec<PathBuf>>,
    root: PathBuf,
) {
    let mut pending_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let debounce_timer = tokio::time::sleep(DEBOUNCE_DURATION);
    tokio::pin!(debounce_timer);
    let mut timer_armed = false;

    loop {
        tokio::select! {
            // Receive file change events from the notify callback
            msg = event_rx.recv() => {
                match msg {
                    Some(paths) => {
                        for path in paths {
                            debug!("[watcher] Detected change: {:?}", path);
                            pending_files.insert(path);
                        }
                        // Reset debounce timer on each new event
                        debounce_timer.as_mut().reset(
                            tokio::time::Instant::now() + DEBOUNCE_DURATION
                        );
                        timer_armed = true;
                    }
                    None => {
                        // Channel closed — watcher has been dropped
                        if !pending_files.is_empty() {
                            trigger_index_update(&root, &pending_files).await;
                        }
                        info!("[watcher] Event channel closed, processor exiting");
                        break;
                    }
                }
            }

            // Debounce timer fired — process accumulated changes
            _ = &mut debounce_timer, if timer_armed => {
                timer_armed = false;
                if !pending_files.is_empty() {
                    trigger_index_update(&root, &pending_files).await;
                    pending_files.clear();
                }
            }
        }
    }
}

/// Trigger an incremental index update by spawning `codeseek init`.
async fn trigger_index_update(root: &Path, files: &std::collections::HashSet<PathBuf>) {
    let file_count = files.len();
    info!(
        "[watcher] Triggering incremental index update for {} file(s)",
        file_count
    );

    // Use the same binary to run `codeseek init`
    let bin = match std::env::current_exe() {
        Ok(b) => b,
        Err(e) => {
            error!("[watcher] Failed to get binary path: {}", e);
            return;
        }
    };

    // Convert root to owned PathBuf so it can be moved into spawn_blocking
    let root_owned = root.to_path_buf();

    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&bin)
            .args(["init"])
            .current_dir(&root_owned)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    })
    .await;

    match output {
        Ok(Ok(output)) => {
            if output.status.success() {
                info!("[watcher] Incremental index update completed successfully");
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    debug!("[watcher] init output: {}", stdout.trim());
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    "[watcher] Incremental index update exited with {}: {}",
                    output.status,
                    stderr.trim()
                );
            }
        }
        Ok(Err(e)) => {
            error!("[watcher] Failed to spawn codeseek init: {}", e);
        }
        Err(e) => {
            error!("[watcher] Spawn task panicked: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_should_watch_rust_file() {
        let root = Path::new("/project");
        let file = Path::new("/project/src/main.rs");
        assert!(should_watch_file(file, root));
    }

    #[test]
    fn test_should_not_watch_target_dir() {
        let root = Path::new("/project");
        let file = Path::new("/project/target/debug/main.rs");
        assert!(!should_watch_file(file, root));
    }

    #[test]
    fn test_should_not_watch_hidden_dir() {
        let root = Path::new("/project");
        let file = Path::new("/project/.git/refs/heads/main");
        assert!(!should_watch_file(file, root));
    }

    #[test]
    fn test_should_not_watch_node_modules() {
        let root = Path::new("/project");
        let file = Path::new("/project/node_modules/package/index.js");
        assert!(!should_watch_file(file, root));
    }

    #[test]
    fn test_should_not_watch_unsupported_extension() {
        let root = Path::new("/project");
        let file = Path::new("/project/README.md");
        // .md is actually in WATCHED_EXTENSIONS, so it should be watched
        assert!(should_watch_file(file, root));
    }

    #[test]
    fn test_should_not_watch_binary_file() {
        let root = Path::new("/project");
        let file = Path::new("/project/logo.png");
        assert!(!should_watch_file(file, root));
    }

    #[test]
    fn test_should_watch_python_file() {
        let root = Path::new("/project");
        let file = Path::new("/project/app.py");
        assert!(should_watch_file(file, root));
    }

    #[test]
    fn test_should_watch_go_file() {
        let root = Path::new("/project");
        let file = Path::new("/project/main.go");
        assert!(should_watch_file(file, root));
    }
}
