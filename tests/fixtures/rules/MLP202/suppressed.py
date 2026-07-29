from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        group.add_updater(lambda m: m.become(m.copy()))  # qual: ignore[MLP202]
        self.add(anchor, group)
        self.play(FadeIn(anchor), run_time=2)
