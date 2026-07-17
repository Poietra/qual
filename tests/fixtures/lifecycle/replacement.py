from manim import Circle, ReplacementTransform, Scene, Square, Transform


class Replace(Scene):
    def construct(self):
        sq = Square()
        circle = Circle()
        self.add(sq)
        self.play(ReplacementTransform(sq, circle))


class PlainTransform(Scene):
    def construct(self):
        sq = Square()
        circle = Circle()
        self.add(sq)
        self.play(Transform(sq, circle))
