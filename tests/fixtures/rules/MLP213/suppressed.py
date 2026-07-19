from manim import *


class Suppressed(ThreeDScene):
    def construct(self):
        surface = Surface(  # manim-lint: ignore[MLP213]
            lambda u, v: (u, v, 0),
            resolution=(32, 32),
        )
        self.play(surface.animate.shift(RIGHT), run_time=3)
