from manim import *


class SuppressedScene(Scene):
    def construct(self):
        broken = MathTex(r"a}b{c")  # manim-lint: ignore[MLR110]
        self.add(broken)
