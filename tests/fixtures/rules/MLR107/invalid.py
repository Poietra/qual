from manim import *

preview = MovingCameraScene()


class RiggedZoom(MovingCameraScene, ExportRig):
    def construct(self):
        self.wait()
