from manim import *


class SuppressedScene(Scene):
    def construct(self):
        circle = Circle()
        self.add(circle)
        circle.shift(OUT)  # qual: ignore[MLR121]
        self.wait()
