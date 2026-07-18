from manim import Scene as Sc, Dot as Dt


class Demo(Sc):
    def construct(self):
        dot = Dt()
        dot.add_updater(lambda m, dt: m.rotate(dt))
        self.wait(1)
