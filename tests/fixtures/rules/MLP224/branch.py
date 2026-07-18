from manim import *


class Demo(Scene):
    def construct(self):
        path = VGroup()
        for _ in range(32):
            path.add(Circle())
        path.add_updater(lambda m: m.point_from_proportion(0.5))
        self.add(path)
        self.play(FadeIn(path), run_time=2)
