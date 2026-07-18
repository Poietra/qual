from manim import *


class Demo(Scene):
    def construct(self):
        title = Text("The quick brown fox jumps over the lazy dog by the river")
        target = MathTex(r"\alpha")
        self.add(title)
        self.play(Transform(title, target))  # manim-lint: ignore[MLP208]
