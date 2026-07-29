from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        self.play(FadeIn(square), FadeIn(dot), lag_ratio=0.5)  # qual: ignore[MLC129]
