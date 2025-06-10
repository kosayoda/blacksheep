//! Plumbing between async service tasks and the Qt thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt::{CxxQtThread, Threading};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

/// Forward every update from `rx` onto the Qt thread and apply it to the
/// QObject. A panic inside `apply` is logged and swallowed instead of
/// unwinding into Qt, which would abort the whole shell.
pub fn forward<T, U, F>(thread: CxxQtThread<T>, mut rx: mpsc::Receiver<U>, apply: F) -> AbortHandle
where
    T: Threading + 'static,
    U: Send + 'static,
    F: Fn(Pin<&mut T>, U) + Send + Sync + 'static,
{
    let apply = Arc::new(apply);
    tokio::spawn(async move {
        while let Some(update) = rx.recv().await {
            let apply = Arc::clone(&apply);
            let queued = thread.queue(move |obj| {
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| apply(obj, update))) {
                    let message = payload
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    tracing::error!("service update panicked on the Qt thread: {message}");
                }
            });
            if queued.is_err() {
                // The QObject has been destroyed.
                break;
            }
        }
    })
    .abort_handle()
}

/// Whether a file-system event means something changed on disk, as opposed
/// to a read. Scanners and reloaders read the files they watch, so reacting
/// to access events would loop.
pub fn is_content_change(kind: &notify::EventKind) -> bool {
    use notify::EventKind;
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

/// Set a read-only `#[qproperty(.., READ, NOTIFY)]` field from Rust and emit
/// its change signal, but only when the value actually changed.
macro_rules! set_property {
    ($obj:expr, $field:ident, $value:expr) => {{
        let value = $value;
        if $obj.$field != value {
            $obj.as_mut().rust_mut().$field = value;
            ::paste::paste! { $obj.as_mut().[<$field _changed>](); }
        }
    }};
}
pub(crate) use set_property;
