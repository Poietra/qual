from manim import *


def good(mob):
    mob.shift(RIGHT)
    return mob


def forgot_return(mob):
    mob.shift(RIGHT)


def bare(mob):
    if mob.width > 1:
        return mob
    return


def obscure(mob):
    return helper_value()


class Callbacks(Scene):
    def construct(self):
        sq = Square()
        self.play(ApplyFunction(good, sq))
        self.play(ApplyFunction(lambda m: None, sq))
        self.play(ApplyFunction(lambda m: m, sq))
        factory = lambda: Square()
