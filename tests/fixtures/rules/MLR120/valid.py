from manim import *


class SafeOrbitScene(ThreeDScene):
    def construct(self):
        self.set_camera_orientation(phi=75 * DEGREES, theta=30 * DEGREES)
        self.move_camera(zoom=2)
        # An explicit None is the documented "leave unchanged" value.
        self.set_camera_orientation(focal_distance=None)
        self.wait()
