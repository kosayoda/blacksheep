//! Power actions through logind, the same calls `systemctl suspend` makes.

use serde::Serialize;
use zbus::proxy;

#[proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Manager {
    fn can_suspend(&self) -> zbus::Result<String>;
    fn can_reboot(&self) -> zbus::Result<String>;
    fn can_power_off(&self) -> zbus::Result<String>;
    fn can_hibernate(&self) -> zbus::Result<String>;
    fn can_hybrid_sleep(&self) -> zbus::Result<String>;

    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    fn hibernate(&self, interactive: bool) -> zbus::Result<()>;
    fn hybrid_sleep(&self, interactive: bool) -> zbus::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Suspend,
    Reboot,
    PowerOff,
    Hibernate,
    HybridSleep,
}

/// What logind allows for this user and machine. `true` covers both "yes"
/// and "challenge", where polkit may ask for authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Capabilities {
    pub suspend: bool,
    pub reboot: bool,
    pub power_off: bool,
    pub hibernate: bool,
    pub hybrid_sleep: bool,
}

fn allowed(answer: zbus::Result<String>) -> bool {
    matches!(answer.as_deref(), Ok("yes") | Ok("challenge"))
}

pub async fn capabilities() -> zbus::Result<Capabilities> {
    let connection = zbus::Connection::system().await?;
    let manager = ManagerProxy::new(&connection).await?;
    Ok(Capabilities {
        suspend: allowed(manager.can_suspend().await),
        reboot: allowed(manager.can_reboot().await),
        power_off: allowed(manager.can_power_off().await),
        hibernate: allowed(manager.can_hibernate().await),
        hybrid_sleep: allowed(manager.can_hybrid_sleep().await),
    })
}

/// Perform the action, letting polkit prompt if it needs to.
pub async fn perform(action: Action) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let manager = ManagerProxy::new(&connection).await?;
    match action {
        Action::Suspend => manager.suspend(true).await,
        Action::Reboot => manager.reboot(true).await,
        Action::PowerOff => manager.power_off(true).await,
        Action::Hibernate => manager.hibernate(true).await,
        Action::HybridSleep => manager.hybrid_sleep(true).await,
    }
}
