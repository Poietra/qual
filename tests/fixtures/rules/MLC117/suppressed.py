from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        anim = square.animate.shift(RIGHT)  # qual: ignore[MLC117]
        square.shift(LEFT)
        self.play(anim)
