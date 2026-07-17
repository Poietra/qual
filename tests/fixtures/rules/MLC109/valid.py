from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        animations = [FadeIn(square), FadeOut(square)]
        group = AnimationGroup(FadeIn(square), FadeOut(square))
        chain = Succession(*animations)
        single = AnimationGroup(FadeIn(square), lag_ratio=0.1)
        self.play(group, chain, single)
