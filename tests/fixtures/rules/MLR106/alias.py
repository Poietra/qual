import math as m
from math import nan as NAN

from manim import *


class Alias(Scene):
    def construct(self):
        d = Dot(m.inf)
        d.shift(NAN)
