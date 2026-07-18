import random

from manim import *


class BranchDependentKeys(Scene):
    """Branch-dependent constructions never prove a cold compile."""

    def construct(self):
        anchor = MathTex(r"k")
        if random.random() > 0.5:
            first = MathTex(r"a")
            second = MathTex(r"b")
            third = MathTex(r"c")
            fourth = MathTex(r"d")
        self.play(Write(anchor))
