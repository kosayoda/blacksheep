//! D-Bus backed services. Each module exposes a long-running task that
//! reconnects with backoff and pushes plain `Serialize` snapshots to the
//! caller; none of them touch Qt.

use std::time::Duration;

pub mod bluetooth;
pub mod login1;
pub mod netlink;
pub mod network;
pub mod mpris;
pub mod notifications;
pub mod tray;
pub mod upower;

pub const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

pub fn backoff(previous: Duration) -> Duration {
    (previous * 2).min(Duration::from_secs(30))
}
