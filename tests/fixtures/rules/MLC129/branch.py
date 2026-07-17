from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        dot = Dot()
        anims = [FadeIn(square), FadeIn(dot)]
        self.play(*anims, lag_ratio=0.5)
