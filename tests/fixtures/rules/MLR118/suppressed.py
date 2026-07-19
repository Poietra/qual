from manim import *


class SuppressedAsset(Scene):
    def construct(self):
        art = SVGMobject("assets/unsupported.svg")  # manim-lint: ignore[MLR118]
        self.add(art)
