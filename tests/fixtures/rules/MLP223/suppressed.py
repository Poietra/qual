from manim import *


class Suppressed(Scene):
    def construct(self):
        circle = Circle(stroke_opacity=0, stroke_width=8)
        self.play(circle.animate.shift(RIGHT))  # manim-lint: ignore[MLP223]
