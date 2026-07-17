from manim import *


class Demo(Scene):
    def construct(self):
        for _ in range(3):
            square = Square()
            self.play(square.animate.shift(RIGHT), square.animate.rotate(PI))
