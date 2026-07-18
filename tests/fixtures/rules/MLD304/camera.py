from manim import *


class TourScene(MovingCameraScene):
    def construct(self):
        square = Square()
        self.add(square)
        # Unconditional camera-frame use in a multi-renderer run: the
        # MovingCamera frame contract only exists under Cairo.
        self.play(self.camera.frame.animate.move_to(square))


class GuardedTour(MovingCameraScene):
    def construct(self):
        square = Square()
        self.add(square)
        # A branch (renderer guard included) ends the certain straight-line
        # prefix: silence.
        if config.renderer == "cairo":
            self.play(self.camera.frame.animate.move_to(square))


class EarlyReturnTour(MovingCameraScene):
    def construct(self):
        square = Square()
        self.add(square)
        if config.renderer == "opengl":
            return
        # Reached only when the early return above did not fire: not an
        # all-paths use, so the rule stays silent.
        self.play(self.camera.frame.animate.move_to(square))


class FramelessTour(MovingCameraScene):
    def construct(self):
        # Subclassing alone is not a divergence certainty: without a
        # camera-frame access the scene renders under either renderer.
        self.add(Square())
        self.wait()


class MixedContract(ThreeDScene, MovingCameraScene):
    def construct(self):
        # Mixed 3D + moving chain: the camera kind is Unknown and is never
        # guessed into a diagnostic.
        self.play(self.camera.frame.animate.shift(LEFT))


class PlainCamera(Scene):
    def construct(self):
        # A plain Scene commits to no MovingCamera frame contract.
        self.add(Square())
        self.wait()
