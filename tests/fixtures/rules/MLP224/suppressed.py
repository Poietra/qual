from manim import *


class Demo(Scene):
    def construct(self):
        anchor = Square()
        path = VGroup(Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle(), Circle())
        path.add_updater(lambda m: m.move_to(m.point_from_proportion(0.5)))  # manim-lint: ignore[MLP224]
        self.add(anchor, path)
        self.play(FadeIn(anchor), run_time=2)
