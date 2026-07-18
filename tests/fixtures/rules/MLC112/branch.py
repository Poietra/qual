from manim import *


def make_updater(tracker):
    return lambda m: m.set_x(tracker.get_value())


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        dot = Dot()
        dot.add_updater(make_updater(tracker))
        self.add(dot)
        self.wait(2)
