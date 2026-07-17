from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        if self.flag:
            sq.generate_target()
        self.play(MoveToTarget(sq))
