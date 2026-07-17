from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: m.shift(dt * RIGHT))
        self.wait(3, frozen_frame=False)
        square.clear_updaters()
        self.wait(3)
        self.wait(1.5, frozen_frame=False)
