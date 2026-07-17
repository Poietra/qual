from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        self.add(square, dot)
        self.play(square.animate.shift(RIGHT), square.animate.set_opacity(0.5))
        self.play(square.animate.shift(RIGHT), dot.animate.rotate(PI))
        self.play(FadeIn(square), FadeOut(dot))
