//! Launch programs as transient services of the user manager, the same call
//! `systemd-run --user` makes. systemd forks and execs the program itself, so
//! it starts in its own cgroup and stopping or restarting blacksheep leaves it
//! running. Its output goes to the journal under the unit's name.
//!
//! The program gets the user manager's environment, not blacksheep's. The
//! session has to import the display variables into the manager, as
//! `niri --session` does.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::OnceCell;
use zbus::proxy;
use zbus::zvariant::{OwnedObjectPath, Value};

#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: Vec<(&str, Value<'_>)>,
        aux: Vec<(&str, Vec<(&str, Value<'_>)>)>,
    ) -> zbus::Result<OwnedObjectPath>;

    /// Without a subscriber the manager does not emit `JobRemoved`.
    fn subscribe(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn job_removed(&self, id: u32, job: OwnedObjectPath, unit: String, result: String) -> zbus::Result<()>;
}

static MANAGER: OnceCell<ManagerProxy<'static>> = OnceCell::const_new();

async fn manager() -> zbus::Result<&'static ManagerProxy<'static>> {
    MANAGER
        .get_or_try_init(|| async {
            let connection = zbus::Connection::session().await?;
            let manager = ManagerProxy::new(&connection).await?;
            manager.subscribe().await?;
            Ok(manager)
        })
        .await
}

/// How long to wait for systemd to report whether the program started.
const START_TIMEOUT: Duration = Duration::from_secs(10);

/// Start `argv` as `app-blacksheep-<app>-<id>.service`, named after the
/// freedesktop convention for launched applications. `path` is the absolute
/// path of the program; `argv[0]` is what the program sees as its name.
///
/// An `Err` means the unit was not created and the program did not start.
/// Once the unit exists, a failure to exec is logged rather than returned:
/// the caller has nothing to fall back to at that point.
pub async fn start_app_service(path: &str, argv: &[String], working_directory: Option<&str>) -> zbus::Result<()> {
    let app = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned());
    let name = format!("app-blacksheep-{}-{}.service", escape(&app), unique());

    let mut properties = vec![
        ("Description", Value::from(format!("{app} launched from blacksheep"))),
        ("ExecStart", Value::from(vec![(path, argv.to_vec(), false)])),
        // The start job fails if exec does, not only if fork does.
        ("Type", Value::from("exec")),
        // Keep the unit, and so the program, alive while anything is left in
        // its cgroup. Wrappers that fork the real program and exit need it.
        ("ExitType", Value::from("cgroup")),
        ("CollectMode", Value::from("inactive-or-failed")),
    ];
    if let Some(dir) = working_directory {
        properties.push(("WorkingDirectory", Value::from(dir)));
    }

    let manager = manager().await?;
    // Listen before starting, or a fast job can finish unseen.
    let mut removed = manager.receive_job_removed().await?;
    let job = manager.start_transient_unit(&name, "fail", properties, Vec::new()).await?;

    let wait = async {
        while let Some(signal) = removed.next().await {
            let Ok(args) = signal.args() else { continue };
            if *args.job == *job {
                return Some(args.result);
            }
        }
        None
    };
    match tokio::time::timeout(START_TIMEOUT, wait).await {
        Ok(Some(result)) if result == "done" => {}
        Ok(Some(result)) => {
            tracing::warn!("{name}: start {result}, see journalctl --user -u {name}");
        }
        Ok(None) => tracing::warn!("{name}: JobRemoved stream ended before the start job finished"),
        Err(_) => tracing::warn!("{name}: start job still running after {START_TIMEOUT:?}"),
    }
    Ok(())
}

/// Unique within this process, and across restarts by including the PID.
fn unique() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("{}x{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Escape one unit-name component: anything outside `[A-Za-z0-9:_.]`, and a
/// leading dot, becomes `\xNN`. Dashes are escaped too, since they separate
/// the components.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, byte) in s.bytes().enumerate() {
        let plain = byte.is_ascii_alphanumeric() || byte == b':' || byte == b'_' || (byte == b'.' && i > 0);
        if plain {
            out.push(byte as char);
        } else {
            out.push_str(&format!("\\x{byte:02x}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_like_systemd() {
        assert_eq!(escape("firefox"), "firefox");
        assert_eq!(escape("org.gnome.Nautilus"), "org.gnome.Nautilus");
        assert_eq!(escape("google-chrome"), "google\\x2dchrome");
        assert_eq!(escape(".hidden"), "\\x2ehidden");
        assert_eq!(escape("a b"), "a\\x20b");
    }
}
