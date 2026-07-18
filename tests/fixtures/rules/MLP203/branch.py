from manim import *


class Demo(Scene):
    def construct(self):
        group = VGroup()
        for _ in range(40):
            group.add(Square())
        group.add_updater(lambda m: m.get_family())
        self.add(group)
        self.play(FadeIn(group), run_time=2)
