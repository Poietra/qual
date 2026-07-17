from manim import FadeIn, FadeOut, Scene, Square


class Fades(Scene):
    def construct(self):
        sq = Square()
        self.play(FadeIn(sq))
        self.play(FadeOut(sq))


class FadeOutFresh(Scene):
    def construct(self):
        sq = Square()
        self.play(FadeOut(sq))
