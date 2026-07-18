from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        target = Circle()
        self.add(sq)
        self.play(Transform(sq, target))
