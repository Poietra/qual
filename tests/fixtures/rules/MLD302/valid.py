import random

import numpy as np
from manim import *


class SeededScene(Scene):
    def construct(self):
        # Literal-seeded local generators are deterministic: never flagged.
        rng = random.Random(42)
        gen = np.random.default_rng(7)
        dot = Dot()
        dot.add_updater(lambda m: m.set_x(rng.uniform(-1.0, 1.0)))
        dot.add_updater(lambda m: m.set_y(float(gen.uniform(-1.0, 1.0))))
        self.add(dot)
        # Cold context: global randomness outside any frame callback is
        # not this rule's territory.
        dot.shift(random.random() * RIGHT)
        self.wait(1)
