import math
from math import inf

from manim import *


class Bad(Scene):
    def construct(self):
        p = Dot(float("inf"))
        q = Dot(point=float("nan"))
        sq = Square()
        sq.shift(math.inf)
        sq.move_to(inf)
        sq.scale(float("-Inf"))
        sq.rotate(math.nan)
        sq.shift(float("inf"))  # manim-lint: ignore[MLR106]
