from manim import *


class Suppressed(Scene):
    def construct(self):
        # manim-lint: ignore[MLP214]
        first = MathTex(r"\alpha")
        second = MathTex(r"\beta")
        third = MathTex(r"\gamma")
        fourth = MathTex(r"\delta")
        self.play(Write(first), Write(second), Write(third), Write(fourth))
