//! `IpcHandler` element and the control socket that drives it.
//!
//! ```qml
//! IpcHandler {
//!     target: "bar"
//!     function toggle() { bar.visible = !bar.visible; return bar.visible }
//! }
//! ```
//!
//! ```sh
//! blacksheep msg bar toggle
//! ```

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qlist.h");
        type QList_QVariant = cxx_qt_lib::QList<cxx_qt_lib::QVariant>;

        include!("blacksheep-qt/cpp/shell.h");
        #[namespace = "blacksheep"]
        fn flatten_js_value(value: &QVariant) -> QVariant;
        #[namespace = "blacksheep"]
        fn invoke_qml_function(
            object: Pin<&mut QObject>,
            name: &QString,
            args: &QList_QVariant,
            ok: &mut bool,
        ) -> QVariant;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[namespace = "blacksheep"]
        /// Name used to address this handler from `blacksheep msg`.
        #[qproperty(QString, target, READ, WRITE = set_target, NOTIFY)]
        type IpcHandler = super::IpcHandlerRust;

        fn set_target(self: Pin<&mut IpcHandler>, target: QString);
    }

    impl cxx_qt::Threading for IpcHandler {}
}

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;

use blacksheep_core::ipc::{Reply, Request};
use cxx_qt::casting::Upcast as _;
use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::{QList, QString, QVariant};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::oneshot;

use crate::service::set_property;
use crate::variant::{value_to_variant, variant_to_value};

static HANDLERS: Mutex<Option<HashMap<String, CxxQtThread<qobject::IpcHandler>>>> =
    Mutex::new(None);

fn handlers()
-> std::sync::MutexGuard<'static, Option<HashMap<String, CxxQtThread<qobject::IpcHandler>>>> {
    HANDLERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
pub struct IpcHandlerRust {
    target: QString,
}

impl Drop for IpcHandlerRust {
    fn drop(&mut self) {
        if let Some(map) = handlers().as_mut() {
            map.remove(&self.target.to_string());
        }
    }
}

impl qobject::IpcHandler {
    fn set_target(mut self: Pin<&mut Self>, target: QString) {
        let previous = self.target.to_string();
        set_property!(self, target, target);
        let thread = self.qt_thread();
        let mut guard = handlers();
        let map = guard.get_or_insert_with(HashMap::new);
        map.remove(&previous);
        let name = self.target.to_string();
        if !name.is_empty() {
            if map.contains_key(&name) {
                tracing::warn!("ipc: duplicate IpcHandler target {name:?}; the newest wins");
            }
            map.insert(name, thread);
        }
    }
}

/// Start the control socket. Runs until the process exits.
pub fn start_server() {
    tokio::spawn(async {
        if let Err(err) = serve().await {
            tracing::error!("ipc: control socket failed: {err}");
        }
    });
}

async fn serve() -> std::io::Result<()> {
    let path = blacksheep_core::ipc::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A previous instance may have died without cleaning up, but a live
    // one (say, a test harness started next to the real shell) must keep
    // its socket: replacing it would cut `blacksheep msg` off from it.
    if path.exists() {
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            tracing::warn!("ipc: another instance owns {path}; not serving the control socket");
            return Ok(());
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = tokio::net::UnixListener::bind(&path)?;
    tracing::info!("ipc: listening on {path}");

    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let reply = match BufReader::new(read).read_line(&mut line).await {
                Ok(_) => match serde_json::from_str::<Request>(&line) {
                    Ok(request) => handle(request).await,
                    Err(err) => Reply::Err {
                        error: format!("invalid request: {err}"),
                    },
                },
                Err(err) => Reply::Err {
                    error: format!("read failed: {err}"),
                },
            };
            let mut out = serde_json::to_string(&reply).unwrap_or_default();
            out.push('\n');
            let _ = write.write_all(out.as_bytes()).await;
        });
    }
}

async fn handle(request: Request) -> Reply {
    match request {
        Request::Reload => {
            crate::shell::request_reload();
            Reply::Ok {
                result: Value::Null,
            }
        }
        Request::List => {
            let mut names: Vec<String> = handlers()
                .as_ref()
                .map(|map| map.keys().cloned().collect())
                .unwrap_or_default();
            names.sort();
            Reply::Ok {
                result: Value::from(names),
            }
        }
        Request::Call {
            target,
            function,
            args,
        } => {
            let Some(thread) = handlers()
                .as_ref()
                .and_then(|map| map.get(&target).cloned())
            else {
                return Reply::Err {
                    error: format!("no IpcHandler with target {target:?}"),
                };
            };
            let (tx, rx) = oneshot::channel();
            let queued = thread.queue(move |handler| {
                let _ = tx.send(invoke(handler, &function, &args));
            });
            if queued.is_err() {
                return Reply::Err {
                    error: format!("IpcHandler {target:?} is gone"),
                };
            }
            match rx.await {
                Ok(Ok(result)) => Reply::Ok { result },
                Ok(Err(error)) => Reply::Err { error },
                Err(_) => Reply::Err {
                    error: "handler dropped the request".into(),
                },
            }
        }
    }
}

/// Runs on the Qt thread.
fn invoke(
    handler: Pin<&mut qobject::IpcHandler>,
    function: &str,
    args: &[Value],
) -> Result<Value, String> {
    let mut list = QList::<QVariant>::default();
    for arg in args {
        list.append(value_to_variant(arg));
    }
    let mut ok = false;
    let object: Pin<&mut cxx_qt::QObject> = handler.upcast_pin();
    let ret = qobject::invoke_qml_function(object, &QString::from(function), &list, &mut ok);
    if ok {
        Ok(variant_to_value(&ret))
    } else {
        Err(format!(
            "no function {function:?} taking {} argument(s) on this IpcHandler",
            args.len()
        ))
    }
}
