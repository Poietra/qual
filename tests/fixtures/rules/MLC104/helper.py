from manim import *


class HelperDemo(Scene):
    def show(self, mob):
        self.play(FadeIn(mob, run_time=0))

    def construct(self):
        self.show(Square())
