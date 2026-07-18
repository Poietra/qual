from manim import RIGHT, Circle, Scene, Square


def spin(mob, dt):
    mob.rotate(dt)


class Demo(Scene):
    def construct(self):
        square = Square()
        other = Circle()
        self.add(square, other)
        square.add_updater(spin)  # manim-lint: ignore[MLP219]
        self.play(other.animate.shift(RIGHT), run_time=2)
        self.play(other.animate.shift(RIGHT), run_time=2)
        self.play(other.animate.shift(RIGHT), run_time=2)
