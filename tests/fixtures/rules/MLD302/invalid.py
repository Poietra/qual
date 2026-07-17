import random
from random import choice

import numpy as np
from manim import *


class JitterScene(Scene):
    def construct(self):
        dot = Dot()
        colors = ["#ff0000", "#00ff00", "#0000ff"]
        dot.add_updater(lambda m: m.set_x(random.uniform(-1.0, 1.0)))
        dot.add_updater(lambda m: m.set_y(np.random.uniform(-1.0, 1.0)))
        dot.add_updater(lambda m: m.set_color(choice(colors)))
        self.add(dot)
        self.wait(2)
