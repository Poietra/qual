from manim import *


class PinnedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)
        # The explicit add pins the membership on both renderers.
        self.add(label)
        label.set_color(RED)
        self.wait()


class RemovedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        # Mutations before the divergent removal are fine.
        label.set_color(RED)
        self.remove_fixed_in_frame_mobjects(label)
        self.remove(label)
        self.wait()
