from manim import Scene, Square


class TimeScaled(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: m.rotate(dt))
        self.wait(4)


class Accumulator(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        square.add_updater(lambda m, dt: m.set_x(m.get_x() + 0.1))
        self.wait(4)
