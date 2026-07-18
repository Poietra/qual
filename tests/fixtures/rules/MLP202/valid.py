from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Circle()
        sq = Square()
        sq.add_updater(lambda m: m.become(m.copy()))
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        snapshot = group.copy()
        self.add(group, snapshot, sq, anchor)
        self.play(FadeIn(anchor), run_time=2)
