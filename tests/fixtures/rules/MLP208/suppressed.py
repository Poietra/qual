from manim import *


class Demo(Scene):
    def construct(self):
        title = Text("hello world")
        target = MathTex(r"\alpha")
        self.add(title)
        self.play(Transform(title, target))  # manim-lint: ignore[MLP208]
