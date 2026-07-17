from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        sq.save_state()
        self.play(sq.animate.shift(RIGHT))
        self.play(Restore(sq))
        sq.restore()
