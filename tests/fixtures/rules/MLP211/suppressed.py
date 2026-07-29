from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        anchor = Circle()
        sq = Square()
        sq.add_updater(lambda m: m.move_to(np.zeros(100000)[:3]))  # qual: ignore[MLP211]
        self.add(anchor, sq)
        self.play(FadeIn(anchor), run_time=2)
