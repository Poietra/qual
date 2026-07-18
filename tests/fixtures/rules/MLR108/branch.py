from manim import *


class BranchScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)
        if config.renderer == "opengl":
            # Branch-dependent mutation is at most Maybe: silence.
            label.set_color(RED)
        self.wait()
