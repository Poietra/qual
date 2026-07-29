from manim import *


class SuppressedAsset(Scene):
    def construct(self):
        art = SVGMobject("assets/unsupported.svg")  # qual: ignore[MLR118]
        self.add(art)
