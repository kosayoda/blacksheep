//! Image providers: `image://icon/<name>` for the icon theme and
//! `image://tray/<id>` for pixels sent by tray items.

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qqmlapplicationengine.h");
        type QQmlApplicationEngine = cxx_qt_lib::QQmlApplicationEngine;
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;

        include!("blacksheep-qt/cpp/icons.h");
        #[namespace = "blacksheep"]
        fn install_icon_providers(engine: Pin<&mut QQmlApplicationEngine>, theme: &QString);
        #[namespace = "blacksheep"]
        fn set_tray_icon(id: &QString, width: i32, height: i32, bgra: &[u8]);
        #[namespace = "blacksheep"]
        fn remove_tray_icon(id: &QString);
    }
}

pub use ffi::{install_icon_providers, remove_tray_icon, set_tray_icon};

/// The icon theme the desktop is configured for, from GTK's settings since a
/// plain Wayland session gives Qt no platform theme to ask.
pub fn desktop_icon_theme() -> String {
    let Some(config) = dirs::config_dir() else {
        return String::new();
    };
    for dir in ["gtk-4.0", "gtk-3.0"] {
        let path = config.join(dir).join("settings.ini");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            if let Some(value) = line.trim().strip_prefix("gtk-icon-theme-name") {
                let value = value.trim_start_matches([' ', '=']).trim().trim_matches('"');
                if !value.is_empty() {
                    return value.to_string();
                }
            }
        }
    }
    String::new()
}
