from manim import ORIGIN, RIGHT, Scene, Square


class SuppressedSuspendedUpdaterResult(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda mob: mob.move_to(ORIGIN))
        self.add(square)
        self.play(square.animate.shift(RIGHT))  # qual: ignore[MLC118]
