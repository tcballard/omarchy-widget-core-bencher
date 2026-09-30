import QtQuick

Item {
    id: root
    required property var widgetContext
    readonly property var theme: widgetContext.theme
    readonly property var metrics: widgetContext.metrics
    readonly property var settings: widgetContext.settings
    readonly property bool dashboard: settings.mode === "dashboard"
    property int ticks: 0
    property bool finished: false
    property var values: []
    readonly property int seed: Math.max(1, Math.min(100, Number(settings.seed) || 1))
    readonly property int durationMs: Math.max(5000, Math.min(180000, Number(settings.durationSeconds) * 1000 || 60000))
    property double startedAt: Date.now()

    // Synthetic data is repeatable and needs no network, commands or user data.
    function valueAt(index) {
        return 0.5 + 0.28 * Math.sin((ticks + index + seed) * 0.19)
                   + 0.15 * Math.cos((ticks + index * 3 + seed) * 0.07)
    }
    function advance() {
        if (Date.now() - startedAt >= durationMs) { finished = true; return; }
        ticks += 1;
        var next = [];
        for (var i = 0; i < 64; ++i) next.push(valueAt(i));
        values = next;
        chart.requestPaint();
    }
    Timer {
        interval: 100; repeat: true
        running: root.dashboard && !root.finished && root.visible
        onTriggered: root.advance()
    }
    readonly property int settingsRevision: Number(widgetContext.settingsRevision) || 0
    onSettingsRevisionChanged: {
        startedAt = Date.now(); ticks = 0; finished = false; values = [];
        deadline.restart();
    }
    Timer {
        id: deadline
        interval: root.durationMs; running: true; repeat: false
        onTriggered: root.finished = true
    }
    Column {
        objectName: "content"
        anchors.fill: parent
        spacing: root.metrics.space(6)
        Text {
            width: parent.width; text: "WIDGET TEST BENCH"
            color: root.theme.muted; font.family: root.metrics.font.family
            font.pixelSize: root.metrics.font.bodySmall; textFormat: Text.PlainText
            elide: Text.ElideRight
        }
        Text {
            width: parent.width
            text: root.finished ? "Workload complete" : root.dashboard ? "Live dashboard" : "Idle baseline"
            color: root.theme.foreground; font.family: root.metrics.font.family
            font.pixelSize: root.metrics.font.body; font.bold: true
            textFormat: Text.PlainText; elide: Text.ElideRight
        }
        Canvas {
            id: chart
            width: parent.width; height: Math.max(30, root.height * 0.28)
            visible: root.dashboard
            onPaint: {
                var ctx = getContext("2d");
                ctx.clearRect(0, 0, width, height);
                ctx.strokeStyle = root.theme.accent; ctx.lineWidth = 2;
                ctx.beginPath();
                for (var i = 0; i < root.values.length; ++i) {
                    var x = i * width / 63;
                    var y = (1 - root.values[i]) * (height - 4) + 2;
                    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
                }
                ctx.stroke();
            }
        }
        Text {
            width: parent.width
            text: root.dashboard ? root.ticks + " updates · 10 Hz · 64 samples" : "Static content · no refresh loop"
            color: root.theme.muted; font.family: root.metrics.font.family
            font.pixelSize: root.metrics.font.bodySmall
            textFormat: Text.PlainText; elide: Text.ElideRight
        }
        Repeater {
            model: root.dashboard ? (root.widgetContext.family === "large" ? 6 : 2) : 0
            delegate: Text {
                required property int index
                width: root.width
                text: "Sensor " + (index + 1) + "   " + Math.round(root.valueAt(index) * 100) + "%"
                color: root.theme.foreground; font.family: root.metrics.font.family
                font.pixelSize: root.metrics.font.bodySmall
                textFormat: Text.PlainText; elide: Text.ElideRight
            }
        }
    }
}
