from manim import *


class CameraZoom(MovingCameraScene):
    def construct(self):
        self.play(self.camera.frame.animate.scale(0.5))
        self.wait()
