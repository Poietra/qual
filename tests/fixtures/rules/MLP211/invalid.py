from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(lambda m: np.zeros(100000))
        sq.add_updater(lambda m: np.zeros((400, 400)))
        self.add(sq)
        self.play(FadeIn(sq), run_time=2)
