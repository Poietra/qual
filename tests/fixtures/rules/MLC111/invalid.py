from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        dot.add_updater(lambda m, dt: m.rotate(dt))
        self.wait(1)
        square = Square()
        square.add_updater(lambda m, dt: m.shift(dt * RIGHT))
        self.add(square)
        self.wait(1)
        self.remove(square)
        self.wait(1)
