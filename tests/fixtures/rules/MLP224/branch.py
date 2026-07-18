from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        path = VGroup()
        for _ in range(32):
            path.add(Circle())
        path.add_updater(lambda m: m.move_to(m.point_from_proportion(0.5)))
        self.add(anchor, path)
        self.play(FadeIn(anchor), run_time=2)
