from manim import RIGHT, Circle, Scene, Square


def spin(mob, dt):
    mob.rotate(dt)


class RemovedEarly(Scene):
    def construct(self):
        square = Square()
        other = Circle()
        self.add(square, other)
        square.add_updater(spin)
        self.play(other.animate.shift(RIGHT), run_time=2)
        square.clear_updaters()
        self.play(other.animate.shift(RIGHT), run_time=2)
        self.play(other.animate.shift(RIGHT), run_time=2)


class ShortSpan(Scene):
    def construct(self):
        square = Square()
        other = Circle()
        self.add(square, other)
        square.add_updater(spin)
        self.play(other.animate.shift(RIGHT), run_time=2)
        self.play(other.animate.shift(RIGHT), run_time=2)
