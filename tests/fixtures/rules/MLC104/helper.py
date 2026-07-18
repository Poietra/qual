from manim import *


class HelperDemo(Scene):
    def show(self, mob):
        self.play(FadeIn(mob, run_time=0))

    def construct(self):
        self.show(Square())


class TwoSiteDemo(Scene):
    def flash(self, mob):
        self.play(mob.animate.shift(RIGHT), run_time=0)

    def construct(self):
        a = Square()
        b = Circle()
        self.flash(a)
        self.flash(b)
