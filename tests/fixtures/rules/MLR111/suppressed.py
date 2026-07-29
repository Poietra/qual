from manim import *


class SuppressedScene(Scene):
    def construct(self):
        dot = Dot()
        self.add(dot)
        self.add_updater(lambda dt: dot.shift(RIGHT * dt))  # qual: ignore[MLR111]
        self.wait(2)
