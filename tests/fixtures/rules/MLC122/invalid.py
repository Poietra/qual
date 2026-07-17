from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(ApplyMethod(square.shift(RIGHT)))
