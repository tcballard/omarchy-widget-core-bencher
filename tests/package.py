#!/usr/bin/env python3
"""Production Core package staging + native Qt rendering; no live desktop claim."""
import json, os, subprocess, sys, tempfile
from pathlib import Path
os.environ.setdefault('QT_QPA_PLATFORM', 'offscreen')
from PySide6.QtCore import QUrl, QMetaObject, Qt
from PySide6.QtGui import QGuiApplication
from PySide6.QtQml import QQmlApplicationEngine
from PySide6.QtTest import QTest
from PySide6.QtQuick import QQuickWindow
root = Path(__file__).resolve().parents[1]
core = Path(sys.argv[1]).resolve()
app = QGuiApplication([])
with tempfile.TemporaryDirectory(prefix='bencher-package-') as tmp:
    temp = Path(tmp)
    env = dict(os.environ, XDG_DATA_HOME=str(temp / 'data'), XDG_STATE_HOME=str(temp / 'state'))
    def cli(*args):
        return json.loads(subprocess.check_output([str(core), *args], env=env, text=True, timeout=15))
    cli('validate', str(root / 'widget'))
    cli('install', str(root / 'widget'))
    staged = Path(cli('list')['catalog'][0]['directory'])
    assert (staged / 'View.qml').read_bytes() == (root / 'widget/View.qml').read_bytes()
    for family in ('small', 'medium', 'large'):
        for light in (False, True):
            for mode in ('idle', 'dashboard'):
                wrapper = temp / 'Preview.qml'
                width, height = {'small': (164, 164), 'medium': (360, 164), 'large': (360, 360)}[family]
                wrapper.write_text('''import QtQuick
import "''' + staged.as_uri() + '''" as Bench
Window {
    id: windowRoot
    width: ''' + str(width) + '''; height: ''' + str(height) + '''; visible: true
    property var context: ({family: "''' + family + '''",
        settingsRevision: 1, settings: {mode:"''' + mode + '''", durationSeconds:5, seed:1},
        theme:{foreground:"''' + ('#222222' if light else '#eeeeee') + '''", muted:"#888888", accent:"#4fa080"},
        metrics:{space:function(n){return n;}, font:{family:"DejaVu Sans", body:12, bodySmall:10}}})
    Bench.View { objectName:"bench"; anchors.fill:parent; widgetContext:windowRoot.context }
    function restart() { context=Object.assign({},context,{settingsRevision:context.settingsRevision+1,settings:{mode:"dashboard",durationSeconds:5,seed:2}}); }
}
''')
                engine = QQmlApplicationEngine()
                warnings = []
                engine.warnings.connect(lambda items: warnings.extend(e.toString() for e in items))
                engine.load(QUrl.fromLocalFile(str(wrapper)))
                assert engine.rootObjects(), (family, light, mode, warnings)
                window = engine.rootObjects()[0]
                from PySide6.QtCore import QObject
                view = window.findChild(QObject, 'bench')
                QTest.qWait(450)
                assert view.property('ticks') > 0 if mode == 'dashboard' else view.property('ticks') == 0
                assert not window.grabWindow().isNull()
                if family == 'small' and not light and mode == 'dashboard':
                    QTest.qWait(4800)
                    assert view.property('finished') is True
                    ticks = view.property('ticks'); QTest.qWait(200)
                    assert view.property('ticks') == ticks
                    QMetaObject.invokeMethod(window, 'restart', Qt.DirectConnection)
                    QTest.qWait(300)
                    assert view.property('finished') is False
                    assert 0 < view.property('ticks') < ticks
                assert not warnings, warnings
                window.close(); engine.deleteLater(); app.processEvents()
    cli('uninstall', 'io.github.tcballard.widget-core-bencher', 'delete')
    assert cli('list')['catalog'] == []
print('PASS: production staging, 12 Qt render combinations, timed stop/rearm and uninstall')
