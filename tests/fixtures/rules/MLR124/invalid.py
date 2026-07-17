from manim import *


class Bad(Scene):
    def construct(self):
        a = Text("<b>bold</b> move")
        b = Text("mix <span foreground='red'>red</span> in")
        c = Text("<i>italic</i>")  # manim-lint: ignore[MLR124]
