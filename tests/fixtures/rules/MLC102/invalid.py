from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(square.shift(RIGHT))
        self.play(square)
        self.play(2)
        self.play("fade")
        self.play(Square())
