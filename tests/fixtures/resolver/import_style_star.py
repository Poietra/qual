from manim import *


class StyleScene(Scene):
    def construct(self):
        square = Square()
        self.play(FadeIn(square), run_time=2)
