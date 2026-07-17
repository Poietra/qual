from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate(run_time=2).shift(RIGHT))
        anim = square.animate.rotate(PI)
        self.play(anim)
