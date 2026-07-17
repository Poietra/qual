from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        anim = square.animate.shift(RIGHT)
        if self.flag:
            square.shift(LEFT)
        self.play(anim)
