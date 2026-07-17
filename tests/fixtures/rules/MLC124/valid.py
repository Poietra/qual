from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        self.add(square, dot)
        self.play(square.animate.shift(RIGHT))
        self.play(square.animate.become(dot))
