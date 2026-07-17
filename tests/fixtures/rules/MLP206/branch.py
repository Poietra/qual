from manim import *

BLINK = 0.004


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(FadeIn(square), run_time=BLINK)
