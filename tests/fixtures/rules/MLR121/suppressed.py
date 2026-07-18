from manim import *


class SuppressedScene(Scene):
    def construct(self):
        circle = Circle()
        self.add(circle)
        circle.shift(OUT)  # manim-lint: ignore[MLR121]
        self.wait()
