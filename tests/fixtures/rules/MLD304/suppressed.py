from manim import *


class SuppressedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)  # manim-lint: ignore[MLD304]
        self.wait()
