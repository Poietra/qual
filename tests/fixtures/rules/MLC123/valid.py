from manim import *


def push_right(mob):
    mob.shift(RIGHT)
    return mob


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(ApplyFunction(push_right, square))
        self.play(ApplyFunction(lambda m: m, square))
