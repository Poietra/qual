from manim import *


class Demo(Scene):
    def construct(self):
        group = VGroup()
        for _ in range(40):
            group.add(Square())
        target = Circle()
        self.add(group)
        self.play(Transform(group, target))
