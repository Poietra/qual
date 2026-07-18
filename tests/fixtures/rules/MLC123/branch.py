from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(ApplyFunction(lambda m: m.shift(RIGHT), square))
