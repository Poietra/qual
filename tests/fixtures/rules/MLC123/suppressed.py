from manim import *


def push_right(mob):
    mob.shift(RIGHT)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(ApplyFunction(push_right, square))  # qual: ignore[MLC123]
