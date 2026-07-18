from manim import *


class Demo(Scene):
    def construct(self):
        arc = Circle()
        arc.add_updater(lambda m: m.point_from_proportion(0.5))
        self.add(arc)
        self.play(FadeIn(arc), run_time=2)
