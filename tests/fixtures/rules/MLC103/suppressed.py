from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(square.shift, RIGHT)  # manim-lint: ignore[MLC103]
