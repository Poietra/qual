from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        buffer = np.zeros(100000)
        sq = Square()
        sq.add_updater(lambda m: np.zeros(3))
        self.add(sq)
        self.play(FadeIn(sq), run_time=2)
