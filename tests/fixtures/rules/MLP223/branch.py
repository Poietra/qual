from manim import *


class BranchOnly(Scene):
    def construct(self, animate_circle):
        circle = Circle(stroke_opacity=0, stroke_width=8)
        if animate_circle:
            self.play(circle.animate.shift(RIGHT))
