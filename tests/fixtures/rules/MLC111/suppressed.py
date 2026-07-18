from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m, dt: m.rotate(dt))
        self.wait(1)  # manim-lint: ignore[MLC111]
