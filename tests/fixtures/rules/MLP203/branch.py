from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        group = VGroup()
        for _ in range(40):
            group.add(Square())
        group.add_updater(lambda m: m.move_to(m.get_family()[0]))
        self.add(anchor, group)
        self.play(FadeIn(anchor), run_time=2)
