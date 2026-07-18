from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        dot = Dot()
        dot.add_updater(lambda m: m.next_to(anchor, UP))
        sq = Square()
        sq.add_updater(lambda m: m.get_family())
        self.add(anchor, dot, sq)
        self.play(FadeIn(anchor), run_time=2)
