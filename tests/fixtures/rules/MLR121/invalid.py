from manim import *


class OverlapScene(Scene):
    def construct(self):
        square = Square()
        circle = Circle()
        self.add(square, circle)
        circle.shift(OUT)
        square.set_z(1)
        self.wait()
