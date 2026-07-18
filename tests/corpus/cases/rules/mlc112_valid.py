from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        dot = Dot()
        dot.add_updater(lambda m, dt: m.set_x(tracker.get_value()))
        self.add(dot)
        self.wait(2)


class Static(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda m: m.set_y(1))
        self.add(square)
        self.wait(2)
