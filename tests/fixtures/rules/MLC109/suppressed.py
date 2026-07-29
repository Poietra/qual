from manim import *


class Demo(Scene):
    def construct(self):
        group = AnimationGroup()  # qual: ignore[MLC109]
        self.play(group)
