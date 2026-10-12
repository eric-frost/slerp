#!/usr/bin/env python3
"""The theme gallery's layout built with Qt Widgets, so a real Breeze
rendering can be put next to the egui one.  Run it under KDE's platform
theme with the same XDG_CONFIG_HOME as the egui gallery:

    QT_QPA_PLATFORMTHEME=kde python3 gallery_qt.py

Needs PyQt6.  Qt has no collapsing header, so the side panel's sections are
group boxes; everything else is the closest stock widget.
"""

import sys

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QAction, QColor, QFont, QIcon, QPainter
from PyQt6.QtWidgets import (
    QAbstractItemView, QApplication, QCheckBox, QComboBox, QDoubleSpinBox, QFormLayout, QGroupBox,
    QHBoxLayout, QHeaderView, QLabel, QLineEdit, QListWidget, QMainWindow, QPushButton, QRadioButton,
    QScrollArea, QSlider, QSplitter, QTableWidget, QTableWidgetItem, QToolBar, QVBoxLayout, QWidget,
)

CLIPS = [("Idle", "2.40 s", "72"), ("Walk", "1.10 s", "33"), ("Run", "0.73 s", "22"),
         ("Jump", "1.50 s", "45"), ("Wave", "3.20 s", "96"), ("Sit down", "2.00 s", "60")]


class FakeView(QWidget):
    """Stands in for the 3D view, painted like the egui gallery's."""

    def paintEvent(self, _event):
        p = QPainter(self)
        r = self.rect()
        p.fillRect(r, QColor(48, 52, 58))
        p.setPen(QColor(60, 65, 72))
        for i in range(12):
            x = round(r.left() + (r.width() - 1) * i / 11)
            p.drawLine(x, r.top(), x, r.bottom())
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(150, 120, 90))
        radius = r.height() * 0.25
        p.drawEllipse(r.center().toPointF(), radius, radius)
        p.setPen(QColor(220, 220, 220))
        p.setFont(QApplication.instance().property("smallFont") or self.font())
        p.drawText(r.adjusted(8, 6, 0, 0), Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignTop, "Orbit · Perspective")


def slider(value, maximum=100):
    s = QSlider(Qt.Orientation.Horizontal)
    s.setRange(0, maximum)
    s.setValue(value)
    return s


def side_panel():
    panel = QWidget()
    layout = QVBoxLayout(panel)

    camera = QGroupBox("Camera")
    form = QFormLayout(camera)
    mode = QComboBox()
    mode.addItems(["Orbit", "First person", "2D pan and zoom"])
    form.addRow("Mode:", mode)
    form.addRow("Field of view:", slider(40))
    ortho = QCheckBox("Orthographic projection")
    grid = QCheckBox("Show ground grid")
    grid.setChecked(True)
    form.addRow(ortho)
    form.addRow(grid)
    form.addRow(QCheckBox("Wireframe overlay"))
    layout.addWidget(camera)

    lighting = QGroupBox("Lighting")
    form = QFormLayout(lighting)
    preset = QComboBox()
    preset.addItems(["Studio", "Sunset sky", "Overcast", "Custom HDR…"])
    preset.setCurrentIndex(1)
    form.addRow("Preset:", preset)
    form.addRow("Exposure:", slider(56))
    radios = QHBoxLayout()
    for i, name in enumerate(["ACES", "AgX", "None"]):
        r = QRadioButton(name)
        r.setChecked(i == 0)
        radios.addWidget(r)
    form.addRow(radios)
    layout.addWidget(lighting)

    animation = QGroupBox("Animation")
    form = QFormLayout(animation)
    buttons = QHBoxLayout()
    buttons.addWidget(QPushButton(QIcon.fromTheme("media-playback-start"), "Play"))
    buttons.addWidget(QPushButton(QIcon.fromTheme("media-playback-stop"), "Stop"))
    loop = QCheckBox("Loop")
    loop.setChecked(True)
    buttons.addWidget(loop)
    form.addRow(buttons)
    form.addRow("Time:", slider(50))
    speed = QDoubleSpinBox()
    speed.setValue(1.0)
    speed.setSuffix("×")
    form.addRow("Speed:", speed)
    clips = QListWidget()
    clips.addItems([f"{n}  ({l})" for n, l, _ in CLIPS[:4]])
    clips.setCurrentRow(1)
    clips.setMaximumHeight(110)
    form.addRow(clips)
    layout.addWidget(animation)

    morphs = QGroupBox("Morph targets")
    form = QFormLayout(morphs)
    for name, v in [("browInnerUp", 0), ("eyeBlinkLeft", 35), ("jawOpen", 80), ("mouthSmile", 10)]:
        form.addRow(name, slider(v))
    form.addRow(QPushButton("Reset All"))
    layout.addWidget(morphs)
    layout.addStretch()

    scroll = QScrollArea()
    scroll.setWidget(panel)
    scroll.setWidgetResizable(True)
    scroll.setFrameShape(QScrollArea.Shape.NoFrame)
    return scroll


def central():
    w = QWidget()
    layout = QVBoxLayout(w)
    row = QHBoxLayout()
    row.addWidget(QLabel("Filter:"))
    edit = QLineEdit()
    edit.setPlaceholderText("Search clips…")
    edit.setFixedWidth(230)
    row.addWidget(edit)
    add = QPushButton("Add Animations…")
    add.setToolTip("Load clips from a GLB, FBX or VRMA file")
    row.addWidget(add)
    row.addStretch()
    layout.addLayout(row)
    layout.addWidget(FakeView(), 1)
    heading = QLabel("Animation clips")
    font = heading.font()
    font.setPointSizeF(font.pointSizeF() * 1.2)
    heading.setFont(font)
    layout.addWidget(heading)
    table = QTableWidget(len(CLIPS), 3)
    table.setHorizontalHeaderLabels(["Name", "Length", "Frames"])
    table.verticalHeader().hide()
    table.setAlternatingRowColors(True)
    table.setSelectionBehavior(QAbstractItemView.SelectionBehavior.SelectRows)
    table.horizontalHeader().setSectionResizeMode(2, QHeaderView.ResizeMode.Stretch)
    table.setColumnWidth(0, 180)
    for r, clip in enumerate(CLIPS):
        for c, text in enumerate(clip):
            table.setItem(r, c, QTableWidgetItem(text))
    table.setFixedHeight(200)
    layout.addWidget(table)
    return w


def main():
    app = QApplication(sys.argv)
    app.setDesktopFileName("slerp-qt-gallery")
    win = QMainWindow()
    win.setWindowTitle("Slerp theme gallery (Qt)")
    menu = win.menuBar()
    file_menu = menu.addMenu("&File")
    for name in ["Open…", "Open Recent", None, "Save Screenshot…", None, "Quit"]:
        file_menu.addSeparator() if name is None else file_menu.addAction(name)
    view = menu.addMenu("&View")
    for name in ["Show Grid", "Wireframe"]:
        a = QAction(name, win)
        a.setCheckable(True)
        view.addAction(a)
    menu.addMenu("&Animation").addAction("Play")
    menu.addMenu("&Help").addAction("About Slerp")

    tools = QToolBar("Main")
    tools.setMovable(False)
    for item in ["document-open", "camera-photo", None, "go-previous", "go-next", None,
                 "zoom-in", "zoom-out", "zoom-fit-best", None, "view-grid", "view-fullscreen"]:
        if item is None:
            tools.addSeparator()
        else:
            tools.addAction(QIcon.fromTheme(item), item)
    win.addToolBar(tools)

    split = QSplitter()
    split.addWidget(side_panel())
    split.addWidget(central())
    split.setSizes([300, 700])
    win.setCentralWidget(split)

    status = win.statusBar()
    for text in ["chair.glb", "12,345 triangles", "4 materials, 9 textures (21.3 MB)"]:
        status.addWidget(QLabel(text))
    small = QLabel("Vulkan · RTX 5070 Ti")
    small.setEnabled(False)
    status.addPermanentWidget(small)

    win.resize(1000, 680)
    win.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
