from manim import *


class Demo(Scene):
    def construct(self):
        title = Text("hello world")
        target = MathTex(r"\alpha + \beta")
        self.add(title)
        self.play(Transform(title, target))
