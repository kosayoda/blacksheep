//! Media players speaking MPRIS2 on the session bus.

use std::collections::HashMap;

use futures_util::{StreamExt as _, stream::select_all};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;
use zbus::fdo::DBusProxy;
use zbus::proxy;
use zbus::zvariant::OwnedValue;

const PREFIX: &str = "org.mpris.MediaPlayer2.";

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Player {
    pub bus_name: String,
    pub identity: String,
    /// `Playing`, `Paused` or `Stopped`.
    pub playback_status: String,
    pub title: String,
    /// Artists joined with ", ".
    pub artist: String,
    pub album: String,
    pub art_url: String,
    pub length_us: i64,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_go_next: bool,
    pub can_go_previous: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

#[proxy(
    interface = "org.mpris.MediaPlayer2",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait MediaPlayer2 {
    #[zbus(property)]
    fn identity(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait PlayerInterface {
    fn play_pause(&self) -> zbus::Result<()>;
    fn play(&self) -> zbus::Result<()>;
    fn pause(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn next(&self) -> zbus::Result<()>;
    fn previous(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
    #[zbus(property)]
    fn can_play(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_pause(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_go_next(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn can_go_previous(&self) -> zbus::Result<bool>;
}

/// Cheap-to-clone handle for sending actions to a player.
#[derive(Clone, Debug)]
pub struct Controller {
    tx: mpsc::UnboundedSender<(String, Action)>,
}

impl Controller {
    pub fn send(&self, bus_name: impl Into<String>, action: Action) {
        let _ = self.tx.send((bus_name.into(), action));
    }
}

/// Start the manager. Snapshots of every known player are sent on `updates`
/// whenever anything changes; the returned controller sends actions.
pub fn start(updates: mpsc::Sender<Vec<Player>>) -> Controller {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(updates, rx));
    Controller { tx }
}

async fn run(updates: mpsc::Sender<Vec<Player>>, mut commands: mpsc::UnboundedReceiver<(String, Action)>) {
    let mut delay = crate::INITIAL_BACKOFF;
    loop {
        match watch(&updates, &mut commands).await {
            Ok(()) => return,
            Err(err) => tracing::warn!("mpris: {err}"),
        }
        tokio::time::sleep(delay).await;
        delay = crate::backoff(delay);
    }
}

enum Event {
    Changed(Player),
    Removed(String),
}

async fn watch(
    updates: &mpsc::Sender<Vec<Player>>,
    commands: &mut mpsc::UnboundedReceiver<(String, Action)>,
) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let dbus = DBusProxy::new(&connection).await?;
    let mut owners = dbus.receive_name_owner_changed().await?;

    let (events_tx, mut events) = mpsc::channel::<Event>(64);
    let mut watchers: HashMap<String, AbortHandle> = HashMap::new();
    let mut players: HashMap<String, Player> = HashMap::new();

    let add = |name: String, watchers: &mut HashMap<String, AbortHandle>| {
        if watchers.contains_key(&name) {
            return;
        }
        let task = tokio::spawn(watch_player(connection.clone(), name.clone(), events_tx.clone()));
        watchers.insert(name, task.abort_handle());
    };

    for name in dbus.list_names().await? {
        let name = name.to_string();
        if name.starts_with(PREFIX) {
            add(name, &mut watchers);
        }
    }

    let result = loop {
        tokio::select! {
            owner = owners.next() => {
                let Some(owner) = owner else {
                    break Err(zbus::Error::Failure("NameOwnerChanged stream ended".into()));
                };
                let args = owner.args()?;
                let name = args.name().to_string();
                if !name.starts_with(PREFIX) {
                    continue;
                }
                if args.new_owner().is_none() {
                    if let Some(task) = watchers.remove(&name) {
                        task.abort();
                    }
                    players.remove(&name);
                    if send_snapshot(updates, &players).await.is_err() {
                        break Ok(());
                    }
                } else {
                    add(name, &mut watchers);
                }
            }
            event = events.recv() => {
                match event {
                    Some(Event::Changed(player)) => {
                        players.insert(player.bus_name.clone(), player);
                    }
                    Some(Event::Removed(name)) => {
                        watchers.remove(&name);
                        players.remove(&name);
                    }
                    None => break Err(zbus::Error::Failure("player events ended".into())),
                }
                if send_snapshot(updates, &players).await.is_err() {
                    break Ok(());
                }
            }
            command = commands.recv() => {
                let Some((name, action)) = command else { break Ok(()) };
                if let Err(err) = perform(&connection, &name, action).await {
                    tracing::warn!("mpris: {action:?} on {name} failed: {err}");
                }
            }
        }
    };

    for task in watchers.values() {
        task.abort();
    }
    result
}

async fn send_snapshot(
    updates: &mpsc::Sender<Vec<Player>>,
    players: &HashMap<String, Player>,
) -> Result<(), mpsc::error::SendError<Vec<Player>>> {
    let mut snapshot: Vec<Player> = players.values().cloned().collect();
    snapshot.sort_by(|a, b| a.bus_name.cmp(&b.bus_name));
    updates.send(snapshot).await
}

async fn perform(connection: &zbus::Connection, name: &str, action: Action) -> zbus::Result<()> {
    let player = PlayerInterfaceProxy::builder(connection)
        .destination(name.to_string())?
        .build()
        .await?;
    match action {
        Action::PlayPause => player.play_pause().await,
        Action::Play => player.play().await,
        Action::Pause => player.pause().await,
        Action::Stop => player.stop().await,
        Action::Next => player.next().await,
        Action::Previous => player.previous().await,
    }
}

/// Follow one player until it disappears.
async fn watch_player(connection: zbus::Connection, name: String, events: mpsc::Sender<Event>) {
    if let Err(err) = watch_player_inner(&connection, &name, &events).await {
        tracing::debug!("mpris: {name}: {err}");
    }
    let _ = events.send(Event::Removed(name)).await;
}

async fn watch_player_inner(
    connection: &zbus::Connection,
    name: &str,
    events: &mpsc::Sender<Event>,
) -> zbus::Result<()> {
    let app = MediaPlayer2Proxy::builder(connection)
        .destination(name.to_string())?
        .build()
        .await?;
    let player = PlayerInterfaceProxy::builder(connection)
        .destination(name.to_string())?
        .build()
        .await?;

    let mut changes = select_all([
        player.receive_playback_status_changed().await.map(|_| ()).boxed(),
        player.receive_metadata_changed().await.map(|_| ()).boxed(),
        player.receive_can_play_changed().await.map(|_| ()).boxed(),
        player.receive_can_pause_changed().await.map(|_| ()).boxed(),
        player.receive_can_go_next_changed().await.map(|_| ()).boxed(),
        player.receive_can_go_previous_changed().await.map(|_| ()).boxed(),
    ]);

    let mut last = None;
    loop {
        let snapshot = read(name, &app, &player).await?;
        if last.as_ref() != Some(&snapshot) {
            if events.send(Event::Changed(snapshot.clone())).await.is_err() {
                return Ok(());
            }
            last = Some(snapshot);
        }
        if changes.next().await.is_none() {
            return Ok(());
        }
    }
}

async fn read(
    name: &str,
    app: &MediaPlayer2Proxy<'_>,
    player: &PlayerInterfaceProxy<'_>,
) -> zbus::Result<Player> {
    let metadata = player.metadata().await.unwrap_or_default();
    let string = |key: &str| {
        metadata
            .get(key)
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_default()
    };
    let artist = metadata
        .get("xesam:artist")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .map(|artists| artists.join(", "))
        .or_else(|| {
            metadata
                .get("xesam:artist")
                .and_then(|v| String::try_from(v.clone()).ok())
        })
        .unwrap_or_default();
    let length_us = metadata
        .get("mpris:length")
        .and_then(|v| i64::try_from(v.clone()).ok())
        .unwrap_or_default();

    Ok(Player {
        bus_name: name.to_string(),
        identity: app.identity().await.unwrap_or_default(),
        playback_status: player.playback_status().await.unwrap_or_default(),
        title: string("xesam:title"),
        artist,
        album: string("xesam:album"),
        art_url: string("mpris:artUrl"),
        length_us,
        can_play: player.can_play().await.unwrap_or_default(),
        can_pause: player.can_pause().await.unwrap_or_default(),
        can_go_next: player.can_go_next().await.unwrap_or_default(),
        can_go_previous: player.can_go_previous().await.unwrap_or_default(),
    })
}
