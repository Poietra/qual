import time

from manim import *


class SuppressedScene(Scene):
    def construct(self):
        label = DecimalNumber(0)
        label.add_updater(lambda m: m.set_value(time.time()))  # manim-lint: ignore[MLD307]
        self.add(label)
        self.wait(1)
