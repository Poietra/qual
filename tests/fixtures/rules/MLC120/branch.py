from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        if self.flag:
            sq.save_state()
        self.play(Restore(sq))
