from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(FadeIn(square), run_time=0.004)  # manim-lint: ignore[MLP206]
