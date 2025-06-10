fn main() {
    // Links Qt and pulls in the exported QML module from blacksheep-qt.
    cxx_qt_build::CxxQtBuilder::new().build();
}
