//! Launch programs that should outlive and not be tied to the shell.

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Launch `argv` as a transient systemd user service, so it runs in its own
/// cgroup with its output in the journal. If the unit cannot be created, for
/// example without a user manager, spawn it directly instead.
///
/// Returns once the launch is under way. Only a program that is not on `PATH`
/// fails here; later failures are logged.
pub fn spawn_detached(argv: &[String], working_directory: Option<&str>) -> std::io::Result<()> {
    let Some(program) = argv.first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let working_directory = working_directory.filter(|d| !d.is_empty()).map(str::to_owned);
    let path = resolve(program, working_directory.as_deref())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("{program}: not found")))?;
    let Some(path) = path.to_str().map(str::to_owned) else {
        return Err(std::io::Error::other(format!("{}: path is not UTF-8", path.display())));
    };

    let argv = argv.to_vec();
    tokio::spawn(async move {
        let started =
            blacksheep_dbus::systemd::start_app_service(&path, &argv, working_directory.as_deref()).await;
        if let Err(err) = started {
            tracing::warn!("cannot start {program} as a systemd service, spawning it directly: {err}", program = argv[0]);
            if let Err(err) = spawn_direct(&path, &argv, working_directory.as_deref()) {
                tracing::warn!("exec {argv:?}: {err}");
            }
        }
    });
    Ok(())
}

/// Spawn in blacksheep's own cgroup: own process group, no inherited stdio,
/// reaped in the background so it never lingers as a zombie.
fn spawn_direct(path: &str, argv: &[String], working_directory: Option<&str>) -> std::io::Result<()> {
    let mut command = std::process::Command::new(path);
    command
        .arg0(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(dir) = working_directory {
        command.current_dir(dir);
    }
    let mut child = tokio::process::Command::from(command).spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// The absolute path `program` runs from, looked up on `PATH` as a shell
/// would. systemd needs an absolute path and does no lookup of its own.
fn resolve(program: &str, working_directory: Option<&str>) -> Option<PathBuf> {
    if program.contains('/') {
        let path = Path::new(program);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            let base = working_directory.map_or_else(std::env::current_dir, |dir| Ok(PathBuf::from(dir))).ok()?;
            base.join(path)
        };
        return is_executable(&path).then_some(path);
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn resolves_on_path() {
        let sh = resolve("sh", None).expect("sh on PATH");
        assert!(sh.is_absolute());
        assert!(resolve("definitely-not-a-program-blacksheep", None).is_none());
        assert_eq!(resolve("/bin/sh", None).as_deref(), Some(std::path::Path::new("/bin/sh")));
    }
}
