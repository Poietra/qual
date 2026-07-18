from manim import *


class Demo(Scene):
    def construct(self):
        sq = Square()
        target = Circle()
        self.add(sq)
        self.play(Transform(sq, target))
        title = Text("Short title")
        label = MathTex(r"\alpha + \beta")
        self.add(title)
        self.play(Transform(title, label))
