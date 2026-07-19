from manim import *


class LargeCairoSurface(ThreeDScene):
    def construct(self):
        surface = Surface(lambda u, v: (u, v, 0), resolution=(32, 32))
        self.play(surface.animate.shift(RIGHT), run_time=3)
