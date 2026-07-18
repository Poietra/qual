from manim import *


class SuppressedOrbit(ThreeDScene):
    def construct(self):
        self.set_camera_orientation(focal_distance=5)  # manim-lint: ignore[MLR120]
        self.wait()
