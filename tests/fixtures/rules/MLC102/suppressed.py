from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(square.shift(RIGHT))  # qual: ignore[MLC102]
