import manim as mn
from manim import SVGMobject as S


class Alias(mn.Scene):
    def construct(self):
        a = mn.SVGMobject("missing.svg")
        b = S("also_missing.svg")
