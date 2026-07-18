from manim import *


class Good(Scene):
    def construct(self, thing, path):
        square = Square()
        img = ImageMobject(path)
        self.play(Create(square))
        self.play(Write(Text("hi")))
        self.play(Create(thing))
        self.play(DrawBorderThenFill(VGroup(square)))
        self.play(FadeIn(img))
        self.play(Uncreate(make_shape()))


def make_shape():
    return Circle()
