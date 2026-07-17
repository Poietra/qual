import random

from manim import *

# A module-level seed makes the global stream reproducible; the rule
# conservatively downgrades the whole file to silence.
random.seed(1234)


class MaybeSeededScene(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m: m.set_x(random.uniform(-1.0, 1.0)))
        self.add(dot)
        self.wait(1)
