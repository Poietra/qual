from manim import *


class SuppressedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)
        label.set_color(RED)  # manim-lint: ignore[MLR108]
        self.wait()
