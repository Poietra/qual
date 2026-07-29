from manim import *


class Suppressed(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(Transform(square, square))  # qual: ignore[MLR113]
