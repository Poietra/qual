from manim import *


class SuppressedScene(Scene):
    def construct(self):
        logo = SVGMobject("C:\\assets\\logo.svg")  # qual: ignore[MLD303]
        self.add(logo)
