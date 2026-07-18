from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate.shift(RIGHT), square.animate.rotate(PI))
        dot = Dot()
        self.play(FadeIn(dot), FadeOut(dot))
