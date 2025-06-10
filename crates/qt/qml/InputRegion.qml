import QtQuick
import Blacksheep

// Limits a window's input to one item's rectangle. Pointer events outside
// it fall through to whatever is under the window, so a surface can stay
// larger than its content: the content animates freely and the surface is
// never resized while shown. Resizing a mapped surface costs a configure
// round trip per change, the compositor repositions it each time, and an
// xdg-popup drifts one pixel each time Qt repositions it.
//
// Window { InputRegion { item: panel } }
QtObject {
    id: root

    /// The item that should receive input; everything else is click-through.
    property Item item: null

    readonly property Window window: item ? item.Window.window : null

    function apply() {
        if (!window || !item)
            return
        const p = item.mapToItem(null, 0, 0)
        Blacksheep.setInputRegion(window, Math.floor(p.x), Math.floor(p.y),
                                  Math.ceil(item.width), Math.ceil(item.height))
    }

    property Connections itemWatch: Connections {
        target: root.item
        function onXChanged() { root.apply() }
        function onYChanged() { root.apply() }
        function onWidthChanged() { root.apply() }
        function onHeightChanged() { root.apply() }
    }
    // The mask only sticks once the platform window exists, so re-apply
    // whenever the window is shown.
    property Connections windowWatch: Connections {
        target: root.window
        function onVisibleChanged() { if (root.window.visible) root.apply() }
    }

    onItemChanged: apply()
    onWindowChanged: apply()
    Component.onCompleted: apply()
}
