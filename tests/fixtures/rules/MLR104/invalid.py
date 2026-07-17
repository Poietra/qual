from manim import *


class Bad(Scene):
    def construct(self):
        a = SVGMobject("missing.svg")
        b = ImageMobject("absent")
        c = SVGMobject("Logo.svg")
        d = ImageMobject("Picture.png")
        e = SVGMobject("gone.svg")  # manim-lint: ignore[MLR104]
