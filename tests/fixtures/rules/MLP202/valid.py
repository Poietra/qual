from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        sq.add_updater(lambda m: m.copy())
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        snapshot = group.copy()
        self.add(group, snapshot, sq)
        self.play(FadeIn(sq), run_time=2)
