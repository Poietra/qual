from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        group = VGroup(square, square)  # qual: ignore[MLC127]
