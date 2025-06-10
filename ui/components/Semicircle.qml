import QtQuick
import QtQuick.Shapes

Item {
    id: root

    // Public API
    property color fillColor: "black"
    property color strokeColor: "black"
    property real  strokeWidth: 0
    property bool  smooth: true
    property real  angle: 0        // center angle, degrees
    property real  chordAngle: 180 // span, degrees

    // Derived geometry (inset to keep stroke on pixel centers)
    readonly property real inset: strokeWidth / 2
    readonly property real cx: width  / 2
    readonly property real cy: height / 2
    readonly property real rx: Math.max(0, width  / 2 - inset)
    readonly property real ry: Math.max(0, height / 2 - inset)
    readonly property real startDeg: angle - chordAngle / 2
    readonly property real endDeg:   angle + chordAngle / 2
    readonly property real startRad: Math.PI * startDeg / 180
    readonly property real endRad:   Math.PI * endDeg   / 180
    readonly property real arcStartX: cx + rx * Math.cos(startRad)
    readonly property real arcStartY: cy + ry * Math.sin(startRad)
    readonly property real arcEndX:   cx + rx * Math.cos(endRad)
    readonly property real arcEndY:   cy + ry * Math.sin(endRad)

    Shape {
        anchors.fill: parent
        antialiasing: root.smooth
        preferredRendererType: Shape.CurveRenderer
        layer.enabled: false

        // Sector fill
        ShapePath {
            strokeWidth: 0
            strokeColor: "transparent"
            fillColor: root.fillColor
            fillRule: ShapePath.WindingFill

            startX: root.arcStartX
            startY: root.arcStartY

            PathAngleArc {
                centerX: root.cx
                centerY: root.cy
                radiusX: root.rx
                radiusY: root.ry
                startAngle: root.startDeg
                sweepAngle: root.chordAngle
            }

            PathLine { x: root.arcStartX; y: root.arcStartY }
        }

        // Arc stroke
        ShapePath {
            strokeWidth: root.strokeWidth
            strokeColor: root.strokeColor
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin

            startX: root.arcStartX
            startY: root.arcStartY

            PathAngleArc {
                centerX: root.cx
                centerY: root.cy
                radiusX: root.rx
                radiusY: root.ry
                startAngle: root.startDeg
                sweepAngle: root.chordAngle
            }
        }

        // 3) Chord stroke
        ShapePath {
            strokeWidth: root.strokeWidth
            strokeColor: root.strokeColor
            fillColor: "transparent"
            capStyle: ShapePath.FlatCap
            joinStyle: ShapePath.BevelJoin

            startX: root.arcEndX
            startY: root.arcEndY
            PathLine { x: root.arcStartX; y: root.arcStartY }
        }
    }
}

