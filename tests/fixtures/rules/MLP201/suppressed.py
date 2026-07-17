from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda m: m.become(Text("hot")))  # manim-lint: ignore[MLP201]
        self.add(square)
        self.play(FadeIn(square), run_time=2)
