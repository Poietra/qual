from manim import *


class RiggedZoom(MovingCameraScene, ExportRig):
    def construct(self):
        # ExportRig is unresolvable, so the camera contract is Unknown:
        # MLR119 must stay silent (the generic MLR107 covers the base).
        self.wait()
