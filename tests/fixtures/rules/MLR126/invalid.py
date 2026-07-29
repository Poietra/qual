from manim import *


class Bad(Scene):
    def construct(self):
        a = Square(fill_opacity=1.5)
        b = Circle(stroke_opacity=-0.1)
        c = Dot(stroke_width=-2)
        sq = Square()
        sq.set_opacity(2.0)
        sq.set_fill(color, 1.2)
        sq.set_stroke(width=-3)
        sq.set_stroke(opacity=1.01)
        sq.set_opacity(-0.5)  # qual: ignore[MLR126]
