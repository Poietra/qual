from manim import *


class OrbitScene(ThreeDScene):
    def construct(self):
        self.set_camera_orientation(phi=75 * DEGREES, focal_distance=5)
        self.move_camera(theta=1.0, focal_distance=8)
        self.wait()
