// OpenUsage in the Omarchy bar. Runs `openusage waybar`, which refreshes the accounts every 5 minutes
// and prints one Waybar-style JSON line per update; the module shows its text and colors. Like the Mac
// status item it has no hover tooltip: left click opens the panel, right click refreshes every account.
import QtQuick
import Quickshell
import Quickshell.Io

Item {
  id: root

  property var bar
  property string moduleName
  property var settings

  readonly property string openusage: Quickshell.env("HOME") + "/.local/bin/openusage"
  readonly property string panel: Quickshell.env("HOME") + "/.local/bin/openusage-panel"

  property string label: "\u{F04C5}"
  property var classes: []

  readonly property bool critical: has("critical") || has("error")
  readonly property bool warning: has("warning")
  readonly property bool stale: has("stale")

  implicitWidth: text.implicitWidth + 15
  implicitHeight: bar ? bar.barSize : 26

  function has(name) {
    return Array.isArray(classes) ? classes.indexOf(name) !== -1 : classes === name
  }

  function update(line) {
    var data
    try {
      data = JSON.parse(line)
    } catch (error) {
      return
    }
    label = data.text || label
    classes = data["class"] || []
  }

  Text {
    id: text
    anchors.centerIn: parent
    text: root.label
    color: root.critical && root.bar ? root.bar.urgent
         : root.warning ? "#d4a017"
         : root.bar ? root.bar.foreground : "white"
    opacity: root.stale ? 0.6 : 1
    font.family: root.bar ? root.bar.fontFamily : "monospace"
    font.pixelSize: 12
  }

  MouseArea {
    id: mouse
    anchors.fill: parent
    acceptedButtons: Qt.LeftButton | Qt.RightButton
    // Launched directly, without a shell: third-party modules get a bar API without `shellQuote`.
    onClicked: function(event) {
      if (event.button === Qt.RightButton)
        Quickshell.execDetached([root.openusage, "waybar", "--refresh"])
      else
        Quickshell.execDetached([root.panel, "--toggle"])
    }
  }

  Process {
    id: daemon
    command: [root.openusage, "waybar"]
    running: true
    stdout: SplitParser {
      onRead: function(line) { root.update(line) }
    }
    // The module exits when another copy already runs or the binary is missing; try again shortly.
    onExited: restart.start()
  }

  Timer {
    id: restart
    interval: 30000
    onTriggered: daemon.running = true
  }
}
