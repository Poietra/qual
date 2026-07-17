import manim as mn
from manim import MathTex as MT


class Alias(mn.Scene):
    def construct(self):
        one = mn.MathTex("\frac{a}{b}")
        two = MT("\alpha")
