from manim import *


class NearMisses(ThreeDScene):
    def construct(self):
        small = Surface(lambda u, v: (u, v, 0), resolution=(16, 16))
        self.play(small.animate.shift(RIGHT), run_time=3)

        unknown_resolution = Surface(
            lambda u, v: (u, v, 0),
            resolution=self.camera.get_phi,
        )
        self.play(unknown_resolution.animate.shift(RIGHT), run_time=3)

        unused = Surface(lambda u, v: (u, v, 0), resolution=(64, 64))
        self.add(unused)
