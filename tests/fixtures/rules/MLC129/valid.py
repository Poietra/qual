from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        self.play(LaggedStart(FadeIn(square), FadeIn(dot), lag_ratio=0.5))
        self.play(FadeIn(square), lag_ratio=0.5)
        self.play(AnimationGroup(FadeIn(square), FadeIn(dot), lag_ratio=0.5))
