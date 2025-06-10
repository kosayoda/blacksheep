import QtQuick
import Blacksheep

Window {
    visible: false
    property int startupVolume: Pipewire.volume
    property int startupPlayers: Mpris.players.count
    property bool startupBattery: Battery.present
    property bool startupNotifications: Notifications.active
    property int startupPrograms: PathExecutables.entries.count
    property int startupTray: Tray.items.count
    property bool startupBluetooth: Bluetooth.available
    property bool startupNetwork: Network.connected
    onStartupVolumeChanged: console.log("volume changed:", startupVolume, Pipewire.muted)

    property Process probe: Process {
        command: ["sh", "-c", "echo process-ok"]
        running: true
        onStdout: line => console.log("process stdout:", line)
        onExited: code => console.log("process exited:", code)
    }
    property IpcHandler ipc: IpcHandler {
        target: "smoke"
        function ping() { return "pong" }
    }

    Repeater {
        model: Niri.workspaces
        delegate: Item {
            Component.onCompleted: console.log("ws delegate:", model.id, idx, output, "active=" + isActive, "focused=" + isFocused)
        }
    }

    Timer {
        interval: 1500
        running: true
        onTriggered: {
            console.log("connected:", Niri.connected)
            console.log("workspaces:", Niri.workspaces.count, JSON.stringify(Niri.workspaces.get(0)))
            console.log("windows:", Niri.windows.count, JSON.stringify(Niri.windows.get(0)))
            console.log("activeWindows:", Niri.activeWindows.count)
            console.log("focused ws/win:", Niri.focusedWorkspaceId, Niri.focusedWindowId)
            console.log("indexOf focused ws:", Niri.workspaces.indexOf(Niri.focusedWorkspaceId))
            console.log("pipewire:", Pipewire.volume, Pipewire.muted)
            console.log("battery:", Battery.present, Battery.percentage, Battery.state, Battery.iconName)
            console.log("battery devices:", Battery.devices.count, JSON.stringify(Battery.devices.get(0)))
            console.log("bluetooth:", "available=" + Bluetooth.available, "powered=" + Bluetooth.powered, Bluetooth.adapterName, "devices:", Bluetooth.devices.count, JSON.stringify(Bluetooth.devices.get(0)))
            console.log("network:", "connected=" + Network.connected, "primary=" + Network.primary, "links:", Network.links.count, JSON.stringify(Network.links.values.map(l => l.name + ":" + l.operational + ":" + l.ipv4)), "wifi:", Network.wifiAvailable, Network.wifiState, Network.ssid, Network.wifiStrength + "%", "networks:", Network.networks.count, JSON.stringify(Network.networks.get(0)))
            console.log("tray:", "active=" + Tray.active, Tray.items.count, JSON.stringify(Tray.items.get(0)).slice(0, 300))
            console.log("path executables:", PathExecutables.entries.count, JSON.stringify(PathExecutables.entries.get(0)))
            console.log("mpris players:", Mpris.players.count, JSON.stringify(Mpris.players.get(0)))
            console.log("notifications active:", Notifications.active, "count:", Notifications.notifications.count, JSON.stringify(Notifications.notifications.get(0)))
            Qt.quit()
        }
    }
}
