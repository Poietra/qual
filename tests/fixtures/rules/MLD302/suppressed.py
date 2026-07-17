import random

from manim import *


class SuppressedScene(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m: m.set_x(random.uniform(-1.0, 1.0)))  # manim-lint: ignore[MLD302]
        self.add(dot)
        self.wait(1)
