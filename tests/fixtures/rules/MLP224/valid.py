from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        arc = Circle()
        arc.add_updater(lambda m: m.move_to(m.point_from_proportion(0.5)))
        self.add(anchor, arc)
        self.play(FadeIn(anchor), run_time=2)
