from manim import *


class Demo(Scene):
    def construct(self):
        group = VGroup(Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square(), Square())
        target = Circle()
        self.add(group)
        self.play(Transform(group, target))  # manim-lint: ignore[MLP207]
