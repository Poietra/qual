import math
from math import inf

from manim import *


def infinity():
    return math.inf


class Good(Scene):
    def construct(self, value):
        sq = Square()
        sq.shift(RIGHT)
        sq.shift(value)
        sq.shift(float(value))
        sq.shift(float("1e30"))
        sq.move_to(infinity())
        big = float("inf")
        sq.shift(big)
        alias = inf
        sq.rotate(math.tau)
        sq.scale(float("inf"))


def float(text):
    # Shadows the builtin: the float("inf") call above can no longer be
    # trusted to produce a non-finite value, so the rule stays silent.
    return 0.0
