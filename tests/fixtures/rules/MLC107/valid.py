from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.generate_target()
        self.play(MoveToTarget(sq))
        circle = Circle()
        self.play(circle.animate.shift(LEFT))
        self.play(MoveToTarget(circle))
