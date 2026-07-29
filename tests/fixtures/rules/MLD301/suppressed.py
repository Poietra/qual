from manim import *


class SuppressedScene(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m: m.shift(0.1 * RIGHT))  # qual: ignore[MLD301]
        self.add(dot)
        self.wait(1)
