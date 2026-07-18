from manim import *


class SuppressedZoom(MovingCameraScene):  # manim-lint: ignore[MLR119]
    def construct(self):
        self.wait()
