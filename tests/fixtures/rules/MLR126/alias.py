import manim as mn
from manim import Circle as Ring


class Alias(mn.Scene):
    def construct(self):
        a = mn.Square(fill_opacity=2)
        b = Ring(stroke_width=-1)
