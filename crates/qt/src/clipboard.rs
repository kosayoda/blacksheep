//! Clipboard access through `arboard`, which works on Wayland (via the
//! data-control protocol, so no focus is needed) and on X11.
//!
//! On Wayland the selection only stays available while the offering
//! process serves it, so the copy runs on its own thread that lives until
//! something else takes the clipboard.

pub fn set_text(text: String) {
    let spawned = std::thread::Builder::new().name("clipboard".into()).spawn(move || {
        use arboard::SetExtLinux as _;
        match arboard::Clipboard::new() {
            Ok(mut clipboard) => {
                if let Err(err) = clipboard.set().wait().text(text) {
                    tracing::warn!("clipboard: {err}");
                }
            }
            Err(err) => tracing::warn!("clipboard: {err}"),
        }
    });
    if let Err(err) = spawned {
        tracing::warn!("clipboard: cannot start thread: {err}");
    }
}
