from manim import *


class TickerScene(Scene):
    def construct(self):
        dot = Dot()
        self.add(dot)
        self.add_updater(lambda dt: dot.shift(RIGHT * dt))
        self.wait(2)
