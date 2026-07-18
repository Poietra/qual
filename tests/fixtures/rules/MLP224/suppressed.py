from manim import *


class Demo(Scene):
    def construct(self):
        path = VGroup(Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle())
        path.add_updater(lambda m: m.point_from_proportion(0.5))  # manim-lint: ignore[MLP224]
        self.add(path)
        self.play(FadeIn(path), run_time=2)
