from manim import *


class Demo(Scene):
    def construct(self):
        group = AnimationGroup()
        chain = Succession(lag_ratio=0.5)
        self.play(group, chain)
