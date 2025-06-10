use blacksheep_core::Result;
use camino::Utf8PathBuf;
use cxx_qt_lib::QGuiApplication;

use clap::Parser;

// Keep the Qt bindings crate linked and run its static initializers so the
// `Blacksheep` QML module is registered before the engine loads anything.
use blacksheep_qt as _;

#[derive(Parser)]
#[command(about, args_conflicts_with_subcommands = true)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Configuration directory, or a QML file to load directly.
    /// Defaults to $XDG_CONFIG_HOME/blacksheep
    config: Option<Utf8PathBuf>,

    /// Do not reload when files in the configuration directory change.
    #[arg(long)]
    no_watch: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Ask the running shell to reload its configuration.
    Reload,
    /// Call a function on an IpcHandler in the running shell.
    Msg {
        /// The IpcHandler's `target`.
        target: String,
        /// Function name declared on the handler.
        function: String,
        /// Arguments; JSON literals are parsed, anything else is passed as a string.
        args: Vec<String>,
    },
    /// List IpcHandler targets in the running shell.
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    blacksheep_core::setup()?;
    tracing::debug!("Debug logging enabled.");

    let args = Args::parse();

    if let Some(command) = args.command {
        return client(command).await;
    }

    // Qt on some distributions logs to journald when stderr is not a tty,
    // which hides QML errors from anyone running the shell by hand.
    // Layer-shell windows only exist on Wayland, so pick that platform
    // unless the caller chose one; Qt's built-in default is xcb.
    for (key, value) in [("QT_FORCE_STDERR_LOGGING", "1"), ("QT_QPA_PLATFORM", "wayland")] {
        if std::env::var_os(key).is_none() {
            // SAFETY: called before any other thread that reads the environment exists.
            unsafe { std::env::set_var(key, value) };
        }
    }

    cxx_qt::init_crate!(blacksheep_qt);
    cxx_qt::init_qml_module!("Blacksheep");

    let mut app = QGuiApplication::new();

    let config = args.config.unwrap_or_else(|| {
        dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
            .and_then(|dir| Utf8PathBuf::from_path_buf(dir).ok())
            .unwrap_or_else(|| Utf8PathBuf::from("."))
            .join("blacksheep")
    });
    tracing::info!("loading configuration from {config}");
    let _shell = blacksheep_qt::shell::start(&config, !args.no_watch);

    if let Some(app) = app.as_mut() {
        app.exec();
    }

    Ok(())
}

async fn client(command: Command) -> Result<()> {
    use blacksheep_core::ipc::{Reply, Request, send};

    let request = match command {
        Command::Reload => Request::Reload,
        Command::List => Request::List,
        Command::Msg {
            target,
            function,
            args,
        } => Request::Call {
            target,
            function,
            args: args
                .into_iter()
                .map(|arg| serde_json::from_str(&arg).unwrap_or(serde_json::Value::String(arg)))
                .collect(),
        },
    };
    match send(&request).await? {
        Reply::Ok { result } => {
            if !result.is_null() {
                use std::io::Write as _;
                // A closed pipe (`| head`) is not an error worth a panic.
                let _ = writeln!(
                    std::io::stdout().lock(),
                    "{}",
                    serde_json::to_string(&result)?
                );
            }
            Ok(())
        }
        Reply::Err { error } => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}
