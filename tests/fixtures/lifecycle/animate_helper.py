from manim import Circle, FadeIn, RED, RIGHT, Scene, Square


class TwoTargets(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        a = Square()
        b = Circle()
        b.set_fill(RED)
        self.go(a)
        self.go(b)


class SameTarget(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        a = Square()
        self.go(a)
        self.go(a)


class LoopedCall(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        a = Square()
        b = Circle()
        for _ in range(3):
            self.go(a)
        self.go(b)


class UnknownArg(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        self.go(make_thing())


class NonLiteralOverride(Scene):
    def construct(self):
        sq = Square()
        t = unknowable()
        self.play(FadeIn(sq, run_time=2), run_time=t)


class SplatOverride(Scene):
    def construct(self):
        sq = Square()
        kw = unknowable()
        self.play(FadeIn(sq, run_time=2), **kw)
