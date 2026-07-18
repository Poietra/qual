from manim import *


class FixedScene(ThreeDScene):
    def construct(self):
        label = Square()
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)


class MovingScene(MovingCameraScene):
    def construct(self):
        self.wait()


class FlatScene(Scene):
    def construct(self):
        self.wait()
