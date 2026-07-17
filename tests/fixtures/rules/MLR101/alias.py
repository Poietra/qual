import manim as mn
from manim import Create as C


class Alias(mn.Scene):
    def construct(self, path):
        img = mn.ImageMobject(path)
        self.play(C(img))
        self.play(mn.Write(5))
