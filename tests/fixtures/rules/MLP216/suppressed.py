from manim import *


class Demo(Scene):
    def construct(self):
        disc = always_redraw(lambda: Circle())  # manim-lint: ignore[MLP216]
        self.add(disc)
        self.play(FadeIn(disc), run_time=2)
