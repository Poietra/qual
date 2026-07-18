from manim import Scene as Sc, Dot as Dt, ValueTracker as VT


class Demo(Sc):
    def construct(self):
        tracker = VT(0)
        dot = Dt()
        dot.add_updater(lambda m: m.set_x(tracker.get_value()))
        self.add(dot)
        self.wait(2)
