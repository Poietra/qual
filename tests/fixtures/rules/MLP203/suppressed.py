from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        group.add_updater(lambda m: m.move_to(m.get_family()[0]))  # qual: ignore[MLP203]
        self.add(anchor, group)
        self.play(FadeIn(anchor), run_time=2)
