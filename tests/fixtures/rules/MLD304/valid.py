from manim import *


class FixedOnlyScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        # Fixing auto-adds identically under both renderers: no divergence.
        self.add_fixed_in_frame_mobjects(label)
        self.wait()
        # Plain Scene.remove has identical membership semantics everywhere.
        self.remove(label)
        self.wait()
