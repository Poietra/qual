from manim import *


class TransparentStroke(Scene):
    def construct(self):
        circle = Circle(fill_opacity=1, stroke_opacity=0, stroke_width=8)
        self.play(circle.animate.shift(RIGHT), run_time=4)


class SetterStroke(Scene):
    def construct(self):
        square = Square().set_stroke(width=5, opacity=0)
        self.play(square.animate.shift(RIGHT), run_time=2)
