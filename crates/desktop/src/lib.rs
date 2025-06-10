//! Installed applications: desktop entries from the XDG data directories,
//! and executables on `PATH`.

use std::os::unix::fs::PermissionsExt as _;

use camino::Utf8PathBuf;
use freedesktop_desktop_entry as fde;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    /// Desktop file id such as `org.mozilla.firefox`.
    pub id: String,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub keywords: Vec<String>,
    pub icon: String,
    /// Exec line split into argv with field codes removed.
    pub exec: Vec<String>,
    pub terminal: bool,
    pub categories: Vec<String>,
    /// Hidden from menus (`NoDisplay`, `Hidden`, or not for this desktop).
    pub no_display: bool,
    pub path: Utf8PathBuf,
}

/// Directories scanned for desktop entries, for callers that want to watch them.
pub fn directories() -> Vec<Utf8PathBuf> {
    fde::default_paths().filter_map(utf8).collect()
}

/// Paths that are not UTF-8 cannot reach QML anyway; drop them with a note.
fn utf8(path: std::path::PathBuf) -> Option<Utf8PathBuf> {
    match Utf8PathBuf::from_path_buf(path) {
        Ok(path) => Some(path),
        Err(path) => {
            tracing::debug!("skipping non-UTF-8 path {}", path.display());
            None
        }
    }
}

/// Parse every application entry, sorted by name. Slow enough to run off
/// the UI thread (tens of milliseconds for a few hundred files).
pub fn scan() -> Vec<Entry> {
    let locales = fde::get_languages_from_env();
    let current_desktop = fde::current_desktop();
    let mut entries: Vec<Entry> = fde::desktop_entries(&locales)
        .into_iter()
        .filter(|entry| entry.type_().unwrap_or("Application") == "Application")
        .filter_map(|entry| {
            let exec = entry.parse_exec().ok()?;
            let shown_here = match (&current_desktop, entry.only_show_in(), entry.not_show_in()) {
                (Some(desktops), Some(only), _) => {
                    only.iter().any(|d| desktops.iter().any(|c| c == d))
                }
                (Some(desktops), _, Some(not)) => {
                    !not.iter().any(|d| desktops.iter().any(|c| c == d))
                }
                _ => true,
            };
            Some(Entry {
                id: entry.id().to_string(),
                name: entry
                    .name(&locales)
                    .map(|s| s.into_owned())
                    .unwrap_or_else(|| entry.id().to_string()),
                generic_name: entry
                    .generic_name(&locales)
                    .map(|s| s.into_owned())
                    .unwrap_or_default(),
                comment: entry
                    .comment(&locales)
                    .map(|s| s.into_owned())
                    .unwrap_or_default(),
                keywords: entry
                    .keywords(&locales)
                    .map(|k| k.into_iter().map(|s| s.into_owned()).collect())
                    .unwrap_or_default(),
                icon: entry.icon().unwrap_or_default().to_string(),
                exec,
                terminal: entry.terminal(),
                categories: entry
                    .categories()
                    .map(|c| c.into_iter().map(String::from).collect())
                    .unwrap_or_default(),
                no_display: entry.no_display() || entry.hidden() || !shown_here,
                path: utf8(entry.path.clone())?,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries.dedup_by(|a, b| a.id == b.id);
    entries
}

/// The argv to run for an entry, wrapping terminal applications.
pub fn launch_argv(entry: &Entry) -> Vec<String> {
    if !entry.terminal {
        return entry.exec.clone();
    }
    let mut argv = terminal_prefix();
    argv.extend(entry.exec.iter().cloned());
    argv
}

fn terminal_prefix() -> Vec<String> {
    if which("xdg-terminal-exec") {
        return vec!["xdg-terminal-exec".into()];
    }
    if let Ok(terminal) = std::env::var("TERMINAL")
        && !terminal.is_empty()
    {
        return vec![terminal, "-e".into()];
    }
    for candidate in ["kitty", "foot", "alacritty", "wezterm", "xterm"] {
        if which(candidate) {
            return vec![candidate.into(), "-e".into()];
        }
    }
    vec!["xterm".into(), "-e".into()]
}

fn which(program: &str) -> bool {
    path_directories()
        .iter()
        .any(|dir| dir.join(program).is_file())
}

/// One program on `PATH`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Executable {
    /// File name, what the user types.
    pub name: String,
    /// Where it resolves to, first hit on `PATH` wins as in a shell.
    pub path: Utf8PathBuf,
}

/// The directories on `PATH`, in order, without duplicates.
pub fn path_directories() -> Vec<Utf8PathBuf> {
    let mut dirs: Vec<Utf8PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path).filter_map(utf8) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Every executable regular file on `PATH`, symlinks followed, one entry per
/// name in shell resolution order, sorted by name.
pub fn scan_path() -> Vec<Executable> {
    let mut found: Vec<Executable> = Vec::new();
    for dir in path_directories() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // metadata() follows symlinks; a dangling link is skipped.
            let Ok(meta) = std::fs::metadata(entry.path()) else {
                continue;
            };
            if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
                continue;
            }
            if found.iter().any(|e| e.name == name) {
                continue;
            }
            found.push(Executable {
                path: dir.join(&name),
                name,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
mod path_tests {
    #[test]
    fn scan_path_finds_sh() {
        let all = super::scan_path();
        let sh = all.iter().find(|e| e.name == "sh").expect("sh on PATH");
        assert!(sh.path.is_file());
        assert!(all.windows(2).all(|w| w[0].name < w[1].name), "sorted and unique");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scan_finds_applications() {
        let dirs = super::directories();
        eprintln!("directories: {dirs:?}");
        let entries = super::scan();
        eprintln!(
            "{} entries; first: {:?}",
            entries.len(),
            entries
                .iter()
                .take(3)
                .map(|e| (&e.id, &e.name, &e.exec))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "visible: {}",
            entries.iter().filter(|e| !e.no_display).count()
        );
    }
}
