from manim import *


class SuppressedZoom(MovingCameraScene):  # qual: ignore[MLR119]
    def construct(self):
        self.wait()
