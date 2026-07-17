from manim import *


class Bad(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(Transform(square, square))
        self.play(ReplacementTransform(square, square))
