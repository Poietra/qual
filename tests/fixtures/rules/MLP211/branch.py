from manim import *
import numpy as np


class Demo(Scene):
    def construct(self):
        n = 100000
        anchor = Circle()
        sq = Square()
        sq.add_updater(lambda m: m.move_to(np.zeros(n)[:3]))
        self.add(anchor, sq)
        self.play(FadeIn(anchor), run_time=2)
