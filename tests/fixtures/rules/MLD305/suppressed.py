from manim import *


class SuppressedScene(Scene):
    def construct(self):
        icon = SVGMobject("ICON")  # manim-lint: ignore[MLD305]
        self.add(icon)
