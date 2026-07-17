import manim as mn


class AliasScene(mn.Scene):
    def construct(self):
        sq = mn.Square()
        self.add(sq)
        self.play(mn.FadeOut(sq))
