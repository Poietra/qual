from manim import *


class Suppressed(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(square.animate)  # manim-lint: ignore[MLR102]
