from manim import *


class Good(Scene):
    def construct(self, level, alpha, color, unknown):
        a = Square(fill_opacity=0.5)
        b = Circle(stroke_opacity=0.0)
        c = Dot(stroke_width=0)
        d = Square(fill_opacity=1)
        sq = Square()
        sq.set_opacity(1.0)
        sq.set_opacity(level)
        sq.set_fill(color, 0.3)
        sq.set_stroke(width=2)
        sq.set_fill(opacity=alpha)
        unknown.set_opacity(5)
