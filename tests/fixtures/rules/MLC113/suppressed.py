from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate.shift(RIGHT)(run_time=2))  # qual: ignore[MLC113]
