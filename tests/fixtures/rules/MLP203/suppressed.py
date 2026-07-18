from manim import *


class Demo(Scene):
    def construct(self):
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        group.add_updater(lambda m: m.get_family())  # manim-lint: ignore[MLP203]
        self.add(group)
        self.play(FadeIn(group), run_time=2)
