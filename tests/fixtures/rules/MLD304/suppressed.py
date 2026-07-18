from manim import *


class SuppressedScene(ThreeDScene):
    def construct(self):
        label = Text("HUD")
        self.add_fixed_in_frame_mobjects(label)
        self.remove_fixed_in_frame_mobjects(label)  # manim-lint: ignore[MLD304]
        self.wait()


class SuppressedTour(MovingCameraScene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(self.camera.frame.animate.move_to(square))  # manim-lint: ignore[MLD304]
