from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m, dt: m.rotate(dt))
        self.add(dot)
        self.wait(1)
        square = Square()
        square.add_updater(lambda m, dt: m.shift(dt * RIGHT))
        self.play(FadeIn(square))
        self.wait(1)
