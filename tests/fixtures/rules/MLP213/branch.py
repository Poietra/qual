from manim import *


class BranchOnly(ThreeDScene):
    def construct(self, animate_surface):
        surface = Surface(lambda u, v: (u, v, 0), resolution=(32, 32))
        if animate_surface:
            self.play(surface.animate.shift(RIGHT), run_time=3)
