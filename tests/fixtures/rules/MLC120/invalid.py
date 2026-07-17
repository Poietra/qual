from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(Restore(sq))
        sq.restore()
