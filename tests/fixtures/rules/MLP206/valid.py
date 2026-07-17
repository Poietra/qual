from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(FadeIn(square), run_time=0.02)
        self.play(FadeIn(square))
        self.wait(0.5)
