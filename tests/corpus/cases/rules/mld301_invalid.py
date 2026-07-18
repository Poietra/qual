from manim import *


class SpinScene(Scene):
    def construct(self):
        dot = Dot()
        square = Square()
        tracker = ValueTracker(0)
        dot.add_updater(lambda m: m.shift(0.1 * RIGHT))
        square.add_updater(lambda m: m.rotate(0.05))
        tracker.add_updater(lambda m: m.increment_value(0.1))

        def grow(mob):
            mob.scale(1.01)

        square.add_updater(grow)
        self.add(dot, square)
        self.wait(2)
