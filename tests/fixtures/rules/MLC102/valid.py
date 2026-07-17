from manim import *


def make_animation(mob):
    return FadeIn(mob)


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(Create(square))
        self.play(square.animate.shift(RIGHT))
        self.play(make_animation(square))
        mystery = build_thing()
        self.play(mystery)
