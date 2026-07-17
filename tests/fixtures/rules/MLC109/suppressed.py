from manim import *


class Demo(Scene):
    def construct(self):
        group = AnimationGroup()  # manim-lint: ignore[MLC109]
        self.play(group)
