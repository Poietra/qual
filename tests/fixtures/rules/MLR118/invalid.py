from manim import *


class UnsupportedAssets(Scene):
    def construct(self):
        first = SVGMobject("assets/unsupported.svg")
        second = SVGMobject("assets/unresolved")
        self.add(first, second)
