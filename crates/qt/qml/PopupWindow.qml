import QtQuick

// A window positioned relative to an item in another window: a menu under a
// tray icon, a calendar under the clock. On Wayland this is an xdg-popup
// parented to the item's surface, so the compositor keeps it on screen and
// closes it when the pointer clicks elsewhere.
//
// PopupWindow { anchorItem: icon; edge: "bottom"; ... }
// then popup.open() / popup.close().
Window {
    id: root

    /// The item to position against.
    property Item anchorItem: null
    /// Which edge of the anchor item the popup hangs off: bottom, top, left or right.
    property string edge: "bottom"
    /// Gap between the anchor item and the popup.
    property int margin: 4
    /// Along the anchor's edge: start, center or end.
    property string align: "start"
    /// Grab input while open, so a click anywhere else closes the popup.
    /// Wayland only allows a grab in response to a click or key press, so
    /// popups opened from a timer or an IPC call must set this to false.
    property bool grab: true

    /// True once the popup has been open long enough to be laid out. Gate
    /// size animations on it, so content that arrives as the popup opens
    /// snaps into place instead of animating from the previous size.
    readonly property bool settled: settle.done

    flags: (grab ? Qt.Popup : Qt.ToolTip) | Qt.FramelessWindowHint
    color: "transparent"
    visible: false
    transientParent: anchorItem ? anchorItem.Window.window : null

    Timer { id: settle; property bool done: false; interval: 100; onTriggered: done = true }
    onVisibleChanged: { settle.stop(); settle.done = false; if (visible) settle.start() }

    function open() {
        if (!anchorItem || !anchorItem.Window.window) {
            console.warn("PopupWindow: anchorItem must be inside a window")
            return
        }
        // Work in the anchor's window-local space, then add the window's
        // own position once. A layer-shell window sits at its screen's
        // offset in the virtual desktop, so that offset must land in the
        // final coordinate exactly once.
        const win = anchorItem.Window.window
        const p = anchorItem.mapToItem(null, 0, 0)
        const w = anchorItem.width, h = anchorItem.height
        let px = p.x, py = p.y
        switch (edge) {
        case "bottom": py = p.y + h + margin; break
        case "top":    py = p.y - height - margin; break
        case "right":  px = p.x + w + margin; break
        case "left":   px = p.x - width - margin; break
        }
        if (edge === "bottom" || edge === "top") {
            if (align === "center") px = p.x + (w - width) / 2
            else if (align === "end") px = p.x + w - width
        } else {
            if (align === "center") py = p.y + (h - height) / 2
            else if (align === "end") py = p.y + h - height
        }
        x = Math.round(win.x + px)
        y = Math.round(win.y + py)
        visible = true
    }

    function close() { visible = false }
    function toggle() { visible ? close() : open() }
}
