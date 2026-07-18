from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(lambda m: np.zeros(100000))  # manim-lint: ignore[MLP211]
        self.add(sq)
        self.play(FadeIn(sq), run_time=2)
