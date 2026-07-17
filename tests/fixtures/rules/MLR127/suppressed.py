from manim import *


class Suppressed(Scene):
    def construct(self):
        eq = MathTex("a^2")
        eq.set_color_by_tex("b^2", RED)  # manim-lint: ignore[MLR127]
        self.add(eq)
