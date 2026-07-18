from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        dot = Dot()
        dot.add_updater(lambda m: m.set_x(tracker.get_value()))
        self.add(dot)
        self.wait(2)
