from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add(dot)  # qual: ignore[MLC110]
