from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        n = 100000
        sq = Square()
        sq.add_updater(lambda m: np.zeros(n))
        self.add(sq)
        self.play(FadeIn(sq), run_time=2)
