import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Shapes

import Blacksheep

import "./components"

Item {
    id: bar
    anchors.fill: parent

    anchors.topMargin: 10

    Clock {
        id: clock
        precision: Clock.Minute
    }




    RowLayout {
        id: modules_left

        height: parent.height
        anchors.left: parent.left

        anchors.leftMargin: 10
        spacing: 5

        Rectangle {
            id: date

            Layout.fillHeight: true
            width: 125

            radius: 10
            color: Theme.light_red

            property bool short_format: true

            Text {
                id: date_text

                anchors.centerIn: parent

                font: Theme.text_font
                color: Theme.dark_gray

                text: Qt.formatDateTime(clock.date, parent.short_format ? "MMM dd · ddd" : "yyyy-MM-dd")
            }

            MouseArea {
                anchors.fill: parent
                onClicked: parent.short_format = !parent.short_format
            }
        }

        Rectangle {
            id: time

            Layout.fillHeight: true
            width: 120

            radius: 10
            color: Theme.light_yellow

            property bool short_format: true

            Text {
                id: time_text

                anchors.centerIn: parent

                font.family: Theme.text_font.family
                font.pointSize: 13

                color: Theme.dark_gray

                text: if (clock.precision == Clock.Second) {
                    Qt.formatDateTime(clock.time, parent.short_format ? "hh:mm:ss" : "hh:mm:ss AP")
                } else {
                    Qt.formatDateTime(clock.time, parent.short_format ? "hh:mm" : "hh mm AP")
                }
            }

            MouseArea {
                anchors.fill: parent

                hoverEnabled: true
                onEntered: clock.precision = Clock.Second
                onExited: clock.precision = Clock.Minute

                onClicked: parent.short_format = !parent.short_format
            }
        }

        Rectangle {
            id: workspaces

            property real padding: 10

            Layout.fillHeight: true
            implicitWidth: childrenRect.width + padding * 2

            radius: 10
            color: Theme.light_blue

            Row {
                spacing: 8
                anchors.verticalCenter: parent.verticalCenter
                anchors.left: parent.left
                anchors.leftMargin: parent.padding

                Repeater {
                    model: Niri.workspaces
                    delegate: Item {

                        Component {
                            id: workspace_dot

                            Rectangle {
                                property bool hover: false

                                width: 16
                                height: width
                                radius: width / 2

                                color: (isUrgent) ? Theme.light_red : "#f2ba6b"

                                MouseArea {
                                    cursorShape: Qt.PointingHandCursor
                                    anchors.fill: parent

                                    hoverEnabled: true
                                    onEntered: parent.hover = true
                                    onExited: parent.hover = false

                                    onClicked: Niri.focusWorkspace(id)
                                }
                            }
                        }

                        Component {
                            id: window_dot

                            Row {
                                spacing: 8
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.left: parent.left
                                anchors.leftMargin: parent.padding

                                Repeater {
                                    id: windows
                                    model: Niri.activeWindows

                                    Semicircle {
                                        property var color: Theme.dark_gray

                                        width: 16
                                        height: width
                                        fillColor: color
                                        strokeWidth: 3
                                        strokeColor: color
                                        angle: 225
                                        chordAngle: (isFocused) ? 360 : 210
                                        Behavior on chordAngle {
                                            NumberAnimation {
                                                duration: 150
                                                easing.type: Easing.OutCubic
                                            }
                                        }

                                        MouseArea {
                                            cursorShape: Qt.PointingHandCursor
                                            anchors.fill: parent

                                            onClicked: Niri.focusWindow(id)
                                        }
                                    }
                                }

                                Rectangle {
                                    width: 16
                                    height: width
                                    radius: width / 2
                                    border.width: 3

                                    color: Theme.dark_gray
                                    visible: windows.count === 0
                                }
                            }
                        }

                        implicitWidth: isFocused ? winLoader.implicitWidth : wsLoader.implicitWidth
                        implicitHeight: 1

                        Behavior on implicitWidth  { NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }

                        // Two separate loaders, both active, stacked
                        Loader {
                            id: wsLoader
                            anchors.centerIn: parent
                            sourceComponent: workspace_dot
                            active: true
                            opacity: isFocused ? 0 : 1
                            scale: isFocused ? 0 : 1
                            Behavior on scale { NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }
                        }

                        Loader {
                            id: winLoader
                            anchors.centerIn: parent
                            sourceComponent: window_dot
                            active: true
                            opacity: isFocused ? 1 : 0
                            scale: isFocused ? 1 : 0
                            Behavior on scale { NumberAnimation { duration: 200; easing.type: Easing.OutCubic } }
                        }
                    }
                }
            }
        }

    }

    RowLayout {
        id: modules_center

        width: parent.width
        height: parent.height

        spacing: 5

        // Row {
        //     Layout.fillHeight: true
        //     Layout.alignment: Qt.AlignHCenter
        //     spacing: 8
        //
        //     Repeater {
        //         model: Niri.activeWindows
        //
        //         Rectangle {
        //             width: 28
        //             height: width
        //             radius: width / 2
        //             anchors.verticalCenter: parent.verticalCenter
        //             border.width: 4.5
        //
        //             property var color_override: {}
        //             color: if (color_override) {
        //                 color_override
        //             } else if (isFocused) {
        //                 Theme.light_yellow
        //             } else {
        //                 "transparent"
        //             }
        //             border.color: if (color_override) {
        //                 color_override
        //             } else {
        //                 Theme.light_magenta
        //             }
        //         // text: Niri.activeWindows.data(Niri.activeWindows.focused_idx, Windows.Title)
        //         }
        //     }
        // }
    }

    RowLayout {
        id: modules_right

        height: parent.height
        anchors.right: parent.right

        anchors.rightMargin: 10
        spacing: 5

        Rectangle {
            id: volume

            Layout.fillHeight: true
            width: 80

            radius: 10
            color: Theme.light_blue

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 10

                Text {
                    Layout.alignment: Qt.AlignCenter
                    font: Theme.icon_font
                    color: Theme.dark_gray

                    text: {
                        var value = Pipewire.volume;
                        var muted = Pipewire.muted;

                        if (muted) {
                            return "󰝟";
                        } else if (value === 0) {
                            return "󰖁";
                        } else if (value <= 33) {
                            return "󰕿";
                        } else if (value <= 66) {
                            return "󰖀";
                        } else {
                            return "󰕾";
                        }
                    }
                }

                Text {
                    Layout.alignment: Qt.AlignCenter
                    font: Theme.text_font
                    color: Theme.dark_gray

                    text: "%1%".arg(Pipewire.volume)
                }
            }
        }
    }
}
