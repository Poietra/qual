from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.shift(LEFT)
        self.play(square.animate.shift(RIGHT))
        anim = square.animate.rotate(PI)
        self.play(anim)
