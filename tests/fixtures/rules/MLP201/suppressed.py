from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Circle()
        square = Square()
        square.add_updater(lambda m: m.become(Text("hot")))  # qual: ignore[MLP201]
        self.add(anchor, square)
        self.play(FadeIn(anchor), run_time=2)
