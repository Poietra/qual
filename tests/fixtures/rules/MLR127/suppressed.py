from manim import *


class Suppressed(Scene):
    def construct(self):
        eq = MathTex("a^2")
        eq.set_color_by_tex("b^2", RED)  # qual: ignore[MLR127]
        self.add(eq)
