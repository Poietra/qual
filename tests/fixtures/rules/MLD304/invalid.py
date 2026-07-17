from manim import *


class FixedLabelScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.wait()
        self.remove_fixed_in_frame_mobjects(label)
        self.wait()
