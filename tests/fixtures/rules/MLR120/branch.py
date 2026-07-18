from manim import *


class ForwardedScene(ThreeDScene):
    def construct(self, **options):
        # focal_distance may or may not be inside the splat: Unknown.
        self.set_camera_orientation(**options)
        self.wait()
