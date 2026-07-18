from manim import *


class Bad(Scene):
    def construct(self, path):
        img = ImageMobject(path)
        self.play(Create(img))
        self.play(Write(5))
        self.play(DrawBorderThenFill(Mobject()))
        self.play(Uncreate(Group(img)))
        self.play(Create(Mobject()))  # manim-lint: ignore[MLR101]
