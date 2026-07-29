from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate.copy())  # qual: ignore[MLC124]
