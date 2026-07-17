from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        group = VGroup(square, square)  # manim-lint: ignore[MLC127]
