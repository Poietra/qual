from manim import *


class GuardedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        # Behind any branch the event is Maybe, never all-paths: a
        # renderer guard (or any other condition) silences the rule.
        if config.renderer == "opengl":
            self.remove_fixed_in_frame_mobjects(label)
        self.wait()
