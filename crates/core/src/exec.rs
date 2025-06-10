//! Launch programs that should outlive and not be tied to the shell.

use std::os::unix::process::CommandExt as _;
use std::process::Stdio;

/// Spawn `argv` detached: own process group, no inherited stdio, reaped in
/// the background so it never lingers as a zombie.
pub fn spawn_detached(argv: &[String], working_directory: Option<&str>) -> std::io::Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(std::io::Error::other("empty command"));
    };
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(dir) = working_directory.filter(|d| !d.is_empty()) {
        command.current_dir(dir);
    }
    let mut child = tokio::process::Command::from(command).spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}
