from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(MoveToTarget(sq))
        dot = Dot()
        self.play(MoveToTarget(dot))
