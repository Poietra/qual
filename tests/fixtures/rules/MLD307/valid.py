import math
import time

from manim import *


class ColdScene(Scene):
    def construct(self):
        # Cold context: construct runs once, not per frame.
        start = time.time()
        with open("config.txt") as handle:
            _ = handle
        label = DecimalNumber(start)
        # Pure math per frame is fine.
        label.add_updater(lambda m: m.set_value(math.sin(1.0)))
        # dt is scene time, the deterministic alternative to wall time.
        label.add_updater(lambda m, dt: m.increment_value(dt))
        self.add(label)
        self.wait(1)
