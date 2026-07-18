from manim import *


def step_size(flag):
    if flag:
        return 0.1
    return 0.0


class BranchScene(Scene):
    def construct(self):
        dot = Dot()
        # The coefficient is a branch-dependent variable, not a literal or
        # Manim constant: the rule cannot prove a fixed step and stays
        # silent.
        step = step_size(True)
        dot.add_updater(lambda m: m.shift(step * RIGHT))
        self.add(dot)
        self.wait(1)
