import QtQuick
import QtQml.Models
import org.kde.layershell as LayerShell
import Blacksheep

// One bar per screen. Qt.application.screens updates on hot-plug, and the
// host reloads this file whenever something under the config directory changes.
QtObject {
    id: root

    property bool barsVisible: true

    property Instantiator bars: Instantiator {
        model: Qt.application.screens

        delegate: Window {
            required property var modelData

            screen: modelData
            visible: root.barsVisible
            color: "transparent"

            LayerShell.Window.scope: "blacksheep-bar"
            LayerShell.Window.anchors: LayerShell.Window.AnchorTop
            LayerShell.Window.layer: LayerShell.Window.LayerTop
            LayerShell.Window.keyboardInteractivity: LayerShell.Window.KeyboardInteractivityNone
            LayerShell.Window.exclusionZone: height

            width: Screen.width
            height: 45

            Loader {
                anchors.fill: parent
                source: Qt.resolvedUrl("Bar.qml")
            }
        }
    }

    // `blacksheep msg bar toggle` hides or shows every bar.
    property IpcHandler ipc: IpcHandler {
        target: "bar"
        function toggle() {
            root.barsVisible = !root.barsVisible
            return root.barsVisible
        }
    }
}
