//! Rust-side constructors for QObjects defined in this crate.
//!
//! Lives in its own bridge so `construct.h` can include the generated
//! `model.cxxqt.h` without an include cycle.

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("blacksheep-qt/cpp/construct.h");

        #[namespace = "blacksheep"]
        type ListModel = crate::model::qobject::ListModel;

        #[namespace = "blacksheep"]
        fn new_list_model() -> UniquePtr<ListModel>;

        #[namespace = "blacksheep"]
        type Shell = crate::shell::qobject::Shell;

        #[namespace = "blacksheep"]
        fn new_shell() -> UniquePtr<Shell>;
    }

    impl UniquePtr<ListModel> {}
    impl UniquePtr<Shell> {}
}

pub use ffi::{new_list_model, new_shell};
