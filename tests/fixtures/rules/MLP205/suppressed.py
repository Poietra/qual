from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(FadeIn(square), run_time=1)
        self.wait(3, frozen_frame=False)  # manim-lint: ignore[MLP205]
