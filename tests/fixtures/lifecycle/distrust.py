from manim import FadeOut, Scene, Square


class MyFade(FadeOut):
    def begin(self):
        pass


class InheritedFade(FadeOut):
    pass


class Distrust(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(MyFade(sq))


class Trusting(Scene):
    def construct(self):
        sq = Square()
        self.add(sq)
        self.play(InheritedFade(sq))
