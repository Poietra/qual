from manim import *


class SuppressedScene(Scene):
    def construct(self):
        icon = SVGMobject("ICON")  # qual: ignore[MLD305]
        self.add(icon)
